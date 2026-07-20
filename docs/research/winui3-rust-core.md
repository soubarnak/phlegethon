# Research: WinUI 3 shell ↔ Rust Download Core

**Ticket:** [#3](https://github.com/soubarnak/phlegethon/issues/3)  
**Scope:** Windows Platform Shell only (Tauri/Debian out of scope for this note). Spec research — no implementation.  
**Constraint:** Primary sources only; no Google-owned documentation.

## Question

What are the viable, maintainable integration patterns for a WinUI 3 (C#/WinRT) Platform Shell to call a shared Rust Download Core (FFI, IPC, embedding), and which pattern best fits queue events, progress streaming, and long-running downloads?

## Verdict (recommended default)

**Default for Phlegethon’s Windows shell: embed the Download Core as a Rust `cdylib` (Windows DLL), expose a small stable C ABI, and consume it from C# via generated P/Invoke (`csbindgen` and/or `[LibraryImport]`), with progress and queue events delivered through a native→managed callback that only enqueues work into `System.Threading.Channels` (UI updates hop to the WinUI dispatcher).**

Use a **sidecar IPC process** (named pipes, optionally gRPC over a local transport) only if process isolation becomes a hard requirement (extractor/runtime crashes must not take down the shell, or the core must update independently of the UI binary).

Do **not** default to UniFFI for C#, and do **not** wrap the core as a WinRT component unless a future requirement forces WinRT-shaped consumption across languages.

---

## Why this shape fits downloads

| Need | Implication |
| --- | --- |
| Long-running transfers | Core owns threads/async runtime; shell must not block the UI thread on FFI calls that wait for I/O. |
| Progress streaming | High-frequency updates → prefer in-process callbacks or a drainable event queue over chatty RPC round-trips. |
| Queue events (enqueued, paused, failed, retry, completed) | Same event channel as progress; typed event envelope + job id. |
| Shared Rust core (later also Tauri) | A **C ABI surface** stays reusable; WinRT-only surfaces do not. |
| Maintainability | Generate bindings; keep the ABI narrow (opaque handles + JSON/bytes payloads if needed); root callbacks for their lifetime. |

---

## Pattern comparison

| Pattern | Process model | Progress / events | Crash isolation | Maintainability for C# | Fit for long downloads |
| --- | --- | --- | --- | --- | --- |
| **A. C ABI + P/Invoke / `LibraryImport`** | In-proc DLL | Callbacks or poll/drain | None (core crash = app crash) | Manual signatures; use `[LibraryImport]` on .NET 7+ | Excellent if non-blocking |
| **B. `csbindgen` (generated P/Invoke)** | In-proc DLL | Same as A; generates `delegate*` / cdecl imports | None | Strong — generates C# from `extern "C"` | Same as A, less drift |
| **C. `cbindgen` headers + hand/gen P/Invoke** | In-proc DLL | Same as A | None | Medium — C header is source of truth for C consumers, extra hop for C# | Same as A |
| **D. Sidecar + named pipes** | Out-of-proc | Length-prefixed frames / stream | Strong | Medium — protocol + process lifecycle | Good; more plumbing |
| **E. Sidecar + gRPC (server streaming)** | Out-of-proc | First-class server streams | Strong | High if already on protobuf | Good; heavier for a desktop-local core |
| **F. UniFFI** | In-proc `cdylib` | Via generated bindings | None | **Weak for C#** — first-party targets are Kotlin/Swift/Python (+ Ruby) | N/A as default |
| **G. WinRT component / CSWinRT wrapping** | In-proc (activation) | WinRT events/async | None | High ceremony; Rust→WinRT is the hard part | Overkill for a download engine |

---

## Pattern details

### A. C ABI FFI + P/Invoke / `LibraryImport`

**How it works.** Rust builds a dynamic system library with `--crate-type=cdylib` (produces a `.dll` on Windows). The crate exports `extern "C"` / `#[no_mangle]` functions. C# loads the DLL and declares matching signatures with `[LibraryImport]` (preferred on modern .NET) or legacy `[DllImport]`.

**Primary sources**

- Rust linkage / `cdylib`: [The Rust Reference — Linkage](https://doc.rust-lang.org/reference/linkage.html) (`cdylib`: “dynamic system library… loaded from another language”).
- FFI discipline: [The Rustonomicon — FFI](https://doc.rust-lang.org/nomicon/ffi.html).
- P/Invoke overview (including callbacks via delegates): [Platform Invoke (P/Invoke)](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke).
- Best practices (`LibraryImport`, function pointers / `UnmanagedCallersOnly`, `SafeHandle`, rooting callbacks): [Native interoperability best practices](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/best-practices).
- WinUI 3 + P/Invoke is an officially documented path: [Build a C# .NET app with WinUI and Win32 interop](https://learn.microsoft.com/en-us/windows/apps/winui/winui3/desktop-winui3-app-with-basic-interop).

**Progress / long work.** Do not block the UI thread inside a P/Invoke that waits for network I/O. Prefer:

1. Start/control APIs that return immediately (`start_job`, `pause_job`, …).
2. A **subscription callback** (or drain API) for events.
3. On the managed side, the callback only writes into a [`System.Threading.Channels`](https://learn.microsoft.com/en-us/dotnet/core/extensions/channels) channel; a UI consumer `await foreach`s and marshals to the dispatcher.

Microsoft’s interop guidance: prefer function pointers / `UnmanagedCallersOnly` over ordinary delegates where possible; if a delegate is passed to native and stored, **root it for the full lifetime** (static field / `GCHandle`) — intermittent crashes otherwise come from GC collecting the callback.

**Pros.** Lowest latency; simplest packaging for a single MSIX/app directory; same DLL model maps cleanly to a shared core used elsewhere via C ABI.  
**Cons.** A panic/`abort` or native AV in the core kills the shell process; ABI and marshalling bugs are UB-adjacent.

---

### B. `csbindgen` (recommended binding generator for A)

**How it works.** Same `cdylib` as A. A Rust `build.rs` runs [Cysharp/csbindgen](https://github.com/Cysharp/csbindgen), which reads `extern "C"` sources and emits C# `DllImport` stubs with **Cdecl** calling convention. For .NET it can emit C# function-pointer (`delegate* unmanaged[Cdecl]<…>`) callback signatures.

**Primary sources**

- Project README / design: [https://github.com/Cysharp/csbindgen](https://github.com/Cysharp/csbindgen) (Rust→C# FFI generator; `crate-type = ["cdylib"]` examples; callback configuration via `csharp_use_function_pointer`).

**Pros.** Removes hand-maintained signature drift; cdecl defaults match Rust `extern "C"`; mature pattern for bringing Rust/C libraries into .NET.  
**Cons.** Generated code today is centered on `[DllImport]`; teams on .NET 7+ may still want a thin hand-written `[LibraryImport]` façade over the hot path, or accept `DllImport` for the core boundary. Still requires a carefully designed C ABI (opaque pointers, explicit free functions, no Rust panic across FFI).

**Fit.** Best **tooling default** on top of pattern A for Phlegethon’s WinUI shell.

---

### C. `cbindgen` + P/Invoke

**How it works.** [mozilla/cbindgen](https://github.com/mozilla/cbindgen) generates a **C header** from Rust. C# still needs P/Invoke declarations (hand-written, or via another generator). Useful when a C/C++ consumer also exists; for C#-only, csbindgen is the shorter path.

**Primary sources**

- [https://github.com/mozilla/cbindgen](https://github.com/mozilla/cbindgen) — “generating C bindings from Rust code”.
- API crate docs: [https://docs.rs/cbindgen](https://docs.rs/cbindgen/latest/cbindgen/).

**Fit.** Secondary. Use if you need a C header for non-.NET tooling; otherwise prefer csbindgen for the WinUI consumer.

---

### D. Sidecar process + named pipes (IPC)

**How it works.** Download Core ships as a `bin` (or hosted service). The WinUI shell starts it and talks over Windows named pipes. .NET exposes this as [`NamedPipeServerStream` / `NamedPipeClientStream`](https://learn.microsoft.com/en-us/dotnet/api/system.io.pipes.namedpipeserverstream) (`System.IO.Pipes`). Rust can own the other end with Windows handle I/O ([`std::os::windows::io`](https://doc.rust-lang.org/std/os/windows/io/index.html)).

**Pros.** Process isolation; independent crash/restart; optional separate update channel for the core. Natural fit if the Extractor Toolchain is itself crash-prone and should not share the UI address space.  
**Cons.** Lifecycle (spawn, handshake, reconnect, orphaned children), framing protocol, privilege/ACL on the pipe name, and higher latency than in-proc callbacks. Progress streaming works well with a **continuous read loop** on a duplex pipe, but you own the codec.

**Fit.** **Escape hatch**, not the first default — unless isolation is decided as a product requirement (related open question on the map: extractor packaging sidecar vs embed).

---

### E. Sidecar + gRPC (server / bidi streaming)

**How it works.** Core hosts a gRPC service; shell is a client. Progress maps cleanly to **server streaming** or **bi-directional streaming** RPCs. ASP.NET Core documents streaming method shapes and recommends `System.Threading.Channels` when multiple threads feed a stream writer.

**Primary sources**

- Hosting: [gRPC services with ASP.NET Core](https://learn.microsoft.com/en-us/aspnet/core/grpc/aspnetcore?view=aspnetcore-9.0).
- Streaming methods: [Create gRPC services and methods](https://learn.microsoft.com/en-us/aspnet/core/grpc/services?view=aspnetcore-9.0) (unary, server/client/bidi streaming; channel pattern for multi-threaded writes).

**Pros.** Strong contracts (protobuf), first-class streaming, cancellation tokens.  
**Cons.** Heavy for a single-user desktop app talking to a localhost core (HTTP/2 stack, codegen, versioning). Rust side needs a gRPC stack as well. Overkill unless you already want a networked companion API.

**Fit.** Viable if IPC is chosen **and** you want a typed IDL; otherwise named pipes + a small framed protocol is leaner for Windows-local-only.

---

### F. UniFFI

**How it works.** Describe an API (UDL / proc-macros); UniFFI generates Rust scaffolding and foreign bindings for a `cdylib`.

**Primary sources**

- User guide: [https://mozilla.github.io/uniffi-rs/latest/](https://mozilla.github.io/uniffi-rs/latest/) — full support for **Kotlin, Swift, and Python**; Ruby also ships; third-party bindings may exist with partial support.
- Motivation / language list: [UniFFI Motivation](https://github.com/mozilla/uniffi-rs/blob/main/docs/manual/src/Motivation.md) (`uniffi-bindgen generate … -l kotlin|swift|python|ruby`); notes that for **one** foreign language, a language-specific tool may be better.

**Fit.** **Not recommended as default for WinUI C#.** First-party C# is absent; Phlegethon’s Windows shell would depend on third-party bindings or still fall back to a C ABI. Prefer csbindgen / hand C ABI for C#, and keep UniFFI off the critical path unless a future multi-language matrix (e.g. mobile) appears.

---

### G. Embedding as WinRT / CSWinRT component

**How it works.** WinRT components expose activatable classes with rich async/event patterns. C# can *author* components with C#/WinRT (`CsWinRTComponent`); consuming ordinary WinRT from C# is natural in WinUI. Authoring a WinRT façade **from Rust** (e.g. via [windows-rs](https://github.com/microsoft/windows-rs)) and packaging/activation for Windows App SDK apps is a large extra surface.

**Primary sources**

- [Walkthrough—Create a C#/WinRT component](https://learn.microsoft.com/en-us/windows/apps/develop/platform/csharp-winrt/create-windows-runtime-component-cswinrt).
- [windows-rs](https://github.com/microsoft/windows-rs) / [windows crate docs](https://microsoft.github.io/windows-docs-rs/doc/windows/) — Rust projections and Windows APIs (useful inside the core for OS calls, not required as the shell boundary).

**Fit.** Poor default. The shell is already C#; the shared core must stay portable to a non-WinRT Linux shell later. A C ABI (or IPC) keeps that door open. WinRT wrapping adds packaging and activation complexity without buying much the shell cannot already do with P/Invoke + Channels.

---

## Recommended event model (applies to A/B; adaptable to D/E)

Regardless of transport, treat the Download Core as an **event producer**:

```text
Shell (WinUI)                         Download Core (Rust)
─────────────                         ───────────────────
start/pause/retry  ──── command ────►  queue + workers
                  ◄──── events ──────  Progress | State | Log
dispatcher ← Channel.Reader
```

**Suggested ABI sketch (informative, not normative):**

- Opaque `CoreHandle` / `JobId` (u64 or pointer + `SafeHandle` on the C# side).
- Commands: `core_create`, `core_destroy`, `job_enqueue`, `job_pause`, `job_resume`, `job_retry`, `job_cancel`.
- Events: register `extern "C" fn(event_type, job_id, payload_ptr, payload_len, user_data)` **or** `events_drain(buf) -> n` polled from a background Task.
- Payload: versioned little-endian structs for hot progress fields; UTF-8 JSON only for rare/rich messages (probe results), to keep progress cheap.
- **Never** unwind a Rust panic across the FFI boundary; catch at the edge and convert to error codes/events ([Rustonomicon FFI](https://doc.rust-lang.org/nomicon/ffi.html) safety model).

On the C# side, Microsoft’s gRPC docs (and Channels docs) already endorse **Channels as the bridge** between producer threads and a single consumer — reuse that pattern even for FFI callbacks: callback → `TryWrite` → UI loop.

---

## Decision matrix for the hand-off spec

| If the hand-off locks… | Choose… |
| --- | --- |
| Single process, shared DLL with Debian/Tauri via C ABI, lowest latency progress | **A + B** (cdylib + csbindgen / LibraryImport) ← **default** |
| Extractor/core crashes must not kill WinUI; core updated separately | **D** (named pipes sidecar); consider **E** if you want protobuf IDL |
| Must expose WinRT to foreign UI languages on Windows only | Revisit **G** (unlikely given Tauri on Linux) |
| Kotlin/Swift mobile clients share the same IDL | Reconsider **F** (UniFFI) for those targets; keep C ABI for C# |

---

## Recommendation for Phlegethon (hand-off pointer)

1. **Windows default:** Rust Download Core as **`cdylib`**, thin **C ABI**, **csbindgen**-generated C# imports (optionally hand-`LibraryImport` wrappers), events via **rooted native callback → `System.Threading.Channels` → WinUI dispatcher**.
2. **Document an IPC fallback** (named pipes duplex stream) as the isolation option when Extractor Toolchain packaging decides “sidecar” or when crash isolation is required.
3. **Out of default scope:** UniFFI-for-C#, WinRT-authored core, gRPC-unless-IPC-is-already-chosen-with-IDL-needs.
4. **Tauri note (non-blocking for this ticket):** the same C ABI (or the same sidecar protocol) should be what the Debian shell calls later — avoid WinRT-shaped APIs in the core.

---

## Sources (primary)

| Topic | URL |
| --- | --- |
| Rust `cdylib` / linkage | https://doc.rust-lang.org/reference/linkage.html |
| Rust FFI | https://doc.rust-lang.org/nomicon/ffi.html |
| .NET P/Invoke | https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke |
| .NET native interop best practices | https://learn.microsoft.com/en-us/dotnet/standard/native-interop/best-practices |
| WinUI 3 + P/Invoke | https://learn.microsoft.com/en-us/windows/apps/winui/winui3/desktop-winui3-app-with-basic-interop |
| `System.Threading.Channels` | https://learn.microsoft.com/en-us/dotnet/core/extensions/channels |
| Named pipes API | https://learn.microsoft.com/en-us/dotnet/api/system.io.pipes.namedpipeserverstream |
| gRPC hosting | https://learn.microsoft.com/en-us/aspnet/core/grpc/aspnetcore?view=aspnetcore-9.0 |
| gRPC streaming methods | https://learn.microsoft.com/en-us/aspnet/core/grpc/services?view=aspnetcore-9.0 |
| csbindgen | https://github.com/Cysharp/csbindgen |
| cbindgen | https://github.com/mozilla/cbindgen |
| UniFFI guide | https://mozilla.github.io/uniffi-rs/latest/ |
| UniFFI motivation / languages | https://github.com/mozilla/uniffi-rs/blob/main/docs/manual/src/Motivation.md |
| C#/WinRT components | https://learn.microsoft.com/en-us/windows/apps/develop/platform/csharp-winrt/create-windows-runtime-component-cswinrt |
| windows-rs | https://github.com/microsoft/windows-rs |
| Rust Windows I/O handles | https://doc.rust-lang.org/std/os/windows/io/index.html |
