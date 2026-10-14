import { describe, expect, it } from "vitest";
import {
  SpeedEstimator,
  displayProgressPercent,
  estimateEtaSeconds,
  formatBytes,
  formatEta,
  formatRate,
} from "./progress";

describe("displayProgressPercent", () => {
  it("is null when the total is unknown", () => {
    expect(displayProgressPercent(1024, null)).toBeNull();
    expect(displayProgressPercent(1024, 0)).toBeNull();
    expect(displayProgressPercent(1024, undefined)).toBeNull();
  });
  it("rounds and never exceeds 100 (merged streams)", () => {
    expect(displayProgressPercent(50, 100)).toBe(50);
    expect(displayProgressPercent(1, 3)).toBe(33);
    expect(displayProgressPercent(137, 100)).toBe(100);
  });
});

describe("formatting", () => {
  it("formats bytes, rates and ETAs", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatBytes(null)).toBe("—");
    expect(formatRate(2 * 1024 * 1024)).toBe("2.0 MB/s");
    expect(formatRate(0)).toBe("");
    expect(formatEta(42)).toBe("42s");
    expect(formatEta(185)).toBe("3m 05s");
    expect(formatEta(3720)).toBe("1h 02m");
    expect(formatEta(null)).toBe("");
  });
  it("estimates remaining time", () => {
    expect(estimateEtaSeconds(100, 300, 50)).toBe(4);
    expect(estimateEtaSeconds(100, null, 50)).toBeNull();
    expect(estimateEtaSeconds(100, 300, 0)).toBeNull();
  });
});

describe("SpeedEstimator", () => {
  it("smooths samples and resets when bytes go backwards", () => {
    const est = new SpeedEstimator(0.5);
    expect(est.sample(0, 0)).toBeNull();
    expect(est.sample(1000, 1000)).toBe(1000);
    expect(est.sample(3000, 2000)).toBe(1500); // 0.5*2000 + 0.5*1000
    expect(est.sample(10, 3000)).toBeNull(); // restart (retry)
    expect(est.sample(1010, 4000)).toBe(1000);
  });
});
