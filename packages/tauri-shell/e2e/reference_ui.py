#!/usr/bin/env python3
"""Drive the real Phlegethon window on the reference URLs (docs/reference-urls.json).

Each URL gets a fresh app with its own HOME, so runs never share state:

  paste the URL -> Add -> (probe sheet: pick items + lowest quality -> Download)
  -> wait for the queue row to say Completed -> check the file with ffprobe.

Needs: a release build (scripts/build-linux.sh), tauri-driver (cargo install tauri-driver),
WebKitWebDriver (webkit2gtk-driver), selenium (pip), Xvfb or a running display, ffprobe,
and yt-dlp (YTDLP_PATH or PATH).

  xvfb-run -a python3 packages/tauri-shell/e2e/reference_ui.py [--site xxxbp] [--out DIR]

The WebKit sandbox stays on. In a container that cannot create its namespaces, set
PHLEGETHON_E2E_NO_SANDBOX=1 to turn it off for this run; a warning is printed.

Prints a table; exit status is non-zero when any row FAILs. A row that fails in its
documented way (blocked_error in the reference file, or YouTube's bot check) is BLOCKED.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from selenium import webdriver
from selenium.common.exceptions import (
    ElementClickInterceptedException,
    NoSuchElementException,
    StaleElementReferenceException,
    TimeoutException,
)
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support.ui import WebDriverWait

ROOT = Path(__file__).resolve().parents[3]
APP = ROOT / "target" / "release" / "phlegethon"
REFERENCE = ROOT / "docs" / "reference-urls.json"
MEDIA_EXT = {".mp4", ".webm", ".mkv", ".m4a", ".mp3", ".mov", ".ts", ".flv"}
HEIGHT_RE = re.compile(r"(\d{3,4})p")


def wait_port(port: int, timeout: float = 20) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.2)
    raise RuntimeError(f"tauri-driver did not open port {port}")


class App:
    """One app instance behind tauri-driver, with an isolated HOME."""

    def __init__(self, home: Path, port: int):
        self.home = home
        env = dict(os.environ)
        env.update(
            HOME=str(home),
            XDG_DATA_HOME=str(home / ".local/share"),
            XDG_CONFIG_HOME=str(home / ".config"),
            XDG_CACHE_HOME=str(home / ".cache"),
            # Tauri only lets WebKitWebDriver attach when this is set.
            TAURI_WEBVIEW_AUTOMATION="true",
            WEBKIT_DISABLE_DMABUF_RENDERER="1",
            WEBKIT_DISABLE_COMPOSITING_MODE="1",
        )
        # Containers usually cannot create the WebKit sandbox namespaces. Turning the
        # sandbox off is opt-in (PHLEGETHON_E2E_NO_SANDBOX=1), never the default (G-19).
        if os.environ.get("PHLEGETHON_E2E_NO_SANDBOX") == "1":
            env["WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS"] = "1"
        driver_bin = shutil.which("tauri-driver") or str(Path.home() / ".cargo/bin/tauri-driver")
        self.proc = subprocess.Popen(
            [driver_bin, "--port", str(port), "--native-port", str(port + 1)],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        # tauri-driver forwards to WebKitWebDriver; both must be listening.
        wait_port(port)
        wait_port(port + 1)
        opts = webdriver.common.options.ArgOptions()
        opts.set_capability("browserName", "wry")
        opts.set_capability("tauri:options", {"application": str(APP)})
        last_error = None
        for _ in range(5):
            try:
                self.driver = webdriver.Remote(command_executor=f"http://127.0.0.1:{port}", options=opts)
                break
            except Exception as e:  # the native driver can still be starting up
                last_error = e
                time.sleep(2)
        else:
            self.proc.terminate()
            raise RuntimeError(f"could not start a WebDriver session: {last_error}")
        self.driver.implicitly_wait(0)

    @property
    def library(self) -> Path:
        return self.home / ".local/share/phlegethon/library"

    def close(self) -> None:
        try:
            self.driver.quit()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


# ---------------------------------------------------------------------------
# UI helpers
# ---------------------------------------------------------------------------


def find(driver, css: str):
    els = driver.find_elements(By.CSS_SELECTOR, css)
    return els[0] if els else None


def wait_for(driver, predicate, timeout: float, what: str):
    try:
        return WebDriverWait(driver, timeout, poll_frequency=0.5).until(lambda d: predicate(d))
    except TimeoutException as e:
        raise TimeoutException(f"timed out after {timeout:.0f}s waiting for {what}") from e


def click(driver, el) -> None:
    try:
        el.click()
    except (ElementClickInterceptedException, StaleElementReferenceException):
        driver.execute_script("arguments[0].click();", el)


def text_of(el) -> str:
    """WebKitWebDriver often reports an empty .text for table content; textContent is reliable."""
    try:
        return " ".join((el.get_attribute("textContent") or "").split())
    except StaleElementReferenceException:
        return ""


def button_with_text(driver, text: str):
    for b in driver.find_elements(By.CSS_SELECTOR, "button"):
        if text_of(b).startswith(text) and b.get_attribute("disabled") is None:
            return b
    for b in driver.find_elements(By.CSS_SELECTOR, "button"):
        if text_of(b).startswith(text):
            return b
    return None


def height_of(label: str) -> int:
    m = HEIGHT_RE.search(label)
    return int(m.group(1)) if m else 10_000


def choose_lowest_quality(driver, item_id: str) -> str:
    """Open the row's quality dropdown (enriches presets on first open) and pick the lowest."""
    css = f"#probe-quality-{item_id}"
    # The first mousedown may swap the dropdown for a skeleton while formats load.
    wrapper = find(driver, f"[data-testid='probe-row-{item_id}'] .ph-probe__quality")
    if wrapper is None:
        return ""
    click(driver, wrapper)
    wait_for(driver, lambda d: find(d, css), 120, f"quality list for {item_id}")
    toggle = find(driver, f"{css} button") or find(driver, css)
    if find(driver, f"{css} .cds--list-box__menu-item") is None:
        click(driver, toggle)
    options = wait_for(
        driver,
        lambda d: d.find_elements(By.CSS_SELECTOR, f"{css} .cds--list-box__menu-item"),
        20,
        "quality options",
    )
    audio_free = [o for o in options if "audio only" not in text_of(o)]
    pick = min(audio_free or options, key=lambda o: height_of(text_of(o)))
    label = text_of(pick)
    click(driver, pick)
    return label


def queue_rows(driver):
    return driver.find_elements(By.CSS_SELECTOR, "[data-testid^='queue-row-']")


def row_status(row) -> str:
    try:
        tag = row.find_element(By.CSS_SELECTOR, "[data-testid='status-tag']")
        return text_of(tag)
    except (NoSuchElementException, StaleElementReferenceException):
        return ""


def row_error(row) -> str:
    try:
        return row.find_element(By.CSS_SELECTOR, "[data-testid='job-error']").get_attribute("title") or ""
    except (NoSuchElementException, StaleElementReferenceException):
        return ""


# ---------------------------------------------------------------------------
# One reference row
# ---------------------------------------------------------------------------


def ffprobe(path: Path) -> tuple[bool, str]:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration:stream=codec_type,codec_name,height",
         "-of", "json", str(path)],
        capture_output=True, text=True, timeout=60,
    )
    if out.returncode != 0:
        return False, out.stderr.strip()[:200]
    data = json.loads(out.stdout or "{}")
    streams = data.get("streams", [])
    kinds = sorted({s.get("codec_type") for s in streams})
    video = next((s for s in streams if s.get("codec_type") == "video"), {})
    duration = float(data.get("format", {}).get("duration") or 0)
    ok = duration > 0 and "video" in kinds
    return ok, f"{duration:.0f}s {video.get('codec_name', '?')} {video.get('height', '?')}p {'+'.join(kinds)}"


def run_entry(entry: dict, home: Path, port: int, shots: Path, max_items: int) -> dict:
    row = {"id": entry["id"], "result": "FAIL", "detail": "", "files": ""}
    blocked_code = (entry.get("expect") or {}).get("blocked_error")
    app = App(home, port)
    d = app.driver
    try:
        box = wait_for(d, lambda d: find(d, "#url-input"), 60, "the URL box")
        box.send_keys(entry["url"])
        click(d, button_with_text(d, "Add"))

        # Probe sheet, probe error, or a direct enqueue straight into the queue.
        outcome = wait_for(
            d,
            lambda d: ("confirm" if find(d, "[data-testid='probe-confirm']") and button_with_text(d, "Download")
                       else "error" if find(d, "[data-testid='probe-error']")
                       else "queued" if queue_rows(d) else None),
            150,
            "probe result",
        )
        if outcome == "error":
            text = text_of(find(d, "[data-testid='probe-error']"))
            cookie_hint = button_with_text(d, "Open cookie settings") is not None
            if cookie_hint and (blocked_code == "needs_browser_cookies" or entry["site"] == "youtube"):
                row.update(result="BLOCKED", detail="needs browser cookies: " + text.replace("\n", " ")[:160])
            else:
                row["detail"] = "probe error: " + text.replace("\n", " ")[:300]
            return row

        picked = []
        expected_jobs = 1
        if outcome == "confirm":
            item_rows = d.find_elements(By.CSS_SELECTOR, "[data-testid^='probe-row-']")
            ids = [r.get_attribute("data-testid")[len("probe-row-"):] for r in item_rows]
            keep = ids[:max_items]
            # Select exactly the first N items.
            if len(ids) > len(keep):
                click(d, button_with_text(d, "Deselect all"))
                for item_id in keep:
                    click(d, find(d, f"label[for='probe-pick-{item_id}']"))
            for item_id in keep:
                picked.append(choose_lowest_quality(d, item_id) or "default")
            confirm = wait_for(
                d,
                lambda d: (b := find(d, "[data-testid='probe-confirm']")) and b.get_attribute("disabled") is None and b,
                60,
                "the Download button",
            )
            label = text_of(confirm)
            m = re.search(r"Download (\d+) selected", label)
            if not m or int(m.group(1)) != len(keep):
                row["detail"] = f"sheet says {label!r} after selecting {len(keep)} item(s)"
                return row
            expected_jobs = int(m.group(1))
            # Close any open dropdown first, then confirm; retry once with a script click.
            click(d, confirm)
            try:
                wait_for(d, lambda d: find(d, "[data-testid='probe-confirm']") is None, 20, "the sheet to close")
            except TimeoutException:
                still = find(d, "[data-testid='probe-confirm']")
                if still is not None:
                    d.execute_script("arguments[0].click();", still)
                wait_for(d, lambda d: find(d, "[data-testid='probe-confirm']") is None, 30, "the sheet to close")

        expected = expected_jobs if outcome == "confirm" else 1
        def settled(d):
            rows = queue_rows(d)
            if len(rows) < expected:
                return None
            states = [row_status(r) for r in rows]
            return states if all(s in ("Completed", "Failed") for s in states) else None

        states = wait_for(d, settled, 900, "downloads to finish")
        errors = [row_error(r) for r in queue_rows(d) if row_status(r) == "Failed"]
        media = [p for p in app.library.rglob("*") if p.is_file() and p.suffix.lower() in MEDIA_EXT]
        checks = [(p, *ffprobe(p)) for p in media]
        row["files"] = "; ".join(f"{p.name[:40]} {info}" for p, _, info in checks)
        if errors:
            code = "needs_browser_cookies" if any("needs_browser_cookies" in e or "403" in e for e in errors) else ""
            if entry["site"] == "youtube" and code:
                row.update(result="BLOCKED", detail="download refused: " + errors[0][:200])
            else:
                row["detail"] = "job failed: " + errors[0][:300]
        elif len(checks) < expected or not all(ok for _, ok, _ in checks):
            row["detail"] = f"{len(checks)} media file(s) for {expected} job(s); states {states}"
        else:
            row.update(result="PASS", detail=f"{len(checks)} file(s), quality {', '.join(picked) or 'file'}")
        return row
    except Exception as e:  # noqa: BLE001 - every failure becomes a FAIL row
        # A site refusing us (bot check, Cloudflare) shows up as an error toast while the
        # sheet loads formats. For rows documented to need cookies that is BLOCKED, not FAIL.
        page = ""
        try:
            page = text_of(d.find_element(By.TAG_NAME, "body"))
        except Exception:
            pass
        refused = "not a bot" in page or "needs_browser_cookies" in page or "Cloudflare" in page
        if refused and (blocked_code == "needs_browser_cookies" or entry["site"] == "youtube"):
            hint = page[page.find("Could not"):][:160] if "Could not" in page else "site refused the request"
            row.update(result="BLOCKED", detail="needs browser cookies: " + hint)
        else:
            row["detail"] = f"{type(e).__name__}: {e}".replace("\n", " ")[:300]
        return row
    finally:
        if row["result"] != "PASS":
            try:
                d.save_screenshot(str(shots / f"{entry['id']}.png"))
            except Exception:
                pass
        app.close()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--site", help="only rows whose site matches")
    ap.add_argument("--id", help="only this row id")
    ap.add_argument("--out", type=Path, help="keep homes, downloads and screenshots here")
    ap.add_argument("--items", type=int, default=2, help="items to download from a listing (default 2)")
    ap.add_argument("--port", type=int, default=4444)
    args = ap.parse_args()

    if os.environ.get("PHLEGETHON_E2E_NO_SANDBOX") == "1":
        print("warning: the WebKit sandbox is disabled for this run (PHLEGETHON_E2E_NO_SANDBOX=1)", file=sys.stderr)
    if not APP.is_file():
        print(f"{APP} is missing: run scripts/build-linux.sh first", file=sys.stderr)
        return 2
    entries = json.loads(REFERENCE.read_text())["entries"]
    entries = [e for e in entries if (not args.site or e["site"] == args.site) and (not args.id or e["id"] == args.id)]
    out = args.out or Path(tempfile.mkdtemp(prefix="phlegethon-e2e-"))
    out.mkdir(parents=True, exist_ok=True)
    shots = out / "screenshots"
    shots.mkdir(exist_ok=True)

    rows = []
    for entry in entries:
        print(f"running {entry['id']} ...", file=sys.stderr, flush=True)
        started = time.time()
        home = out / entry["id"]
        home.mkdir(parents=True, exist_ok=True)
        r = run_entry(entry, home, args.port, shots, args.items)
        r["secs"] = time.time() - started
        print(f"  {r['result']} {r['detail']}", file=sys.stderr, flush=True)
        rows.append(r)

    print("| id | result | secs | detail | files |")
    print("| --- | --- | --- | --- | --- |")
    for r in rows:
        cells = [r["id"], r["result"], f"{r['secs']:.0f}", r["detail"], r["files"]]
        print("| " + " | ".join(c.replace("|", "\\|") for c in cells) + " |")
    print(f"\nartifacts: {out}", file=sys.stderr)
    return 1 if any(r["result"] == "FAIL" for r in rows) else 0


if __name__ == "__main__":
    sys.exit(main())
