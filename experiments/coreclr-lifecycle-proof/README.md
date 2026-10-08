# Worker lifecycle proof

RUG-519 worker stop/restart and buffered-result cleanup pass on the exact installed Editors below. The Unity 7 execute tool refuses unavailable dynamic execution before Roslyn compilation. The three native windows remain unchanged: their reload/adoption order still needs a GUI proof, and first-launch software terms block that proof until the user reviews them.

| Editor | Fixed source | Original source | Only unread-result cleanup removed |
| --- | --- | --- | --- |
| `6000.3.9f1_7a9955a4f2fa` | Six pass; CoreCLR guard skips | Both restart cases fail; both direct semantic checks pass | Both stale-result cases fail after a real semantic result is buffered |
| `7000.0.0a7_581996e1a8f7` | All seven pass | Both restart cases and execute guard fail; both direct semantic checks pass | Both stale-result cases fail after a real semantic result is buffered |

Each corrected run ended normally: fixed exit 0, expected negative controls exit 2, a main-PID quitting marker, and zero remaining owned Editor/import-worker processes. `passed:true` on a negative control means the expected regression was reproduced; its NUnit cases remain `Failed`.

The tests invoke the existing private stop/initialization entrypoints while static state is retained. They cover repeated stop, repeated initialization, restart, semantic completion/signature output, and unread-result rejection at `Submit`/`TryTake`. They do not prove that this Unity 7 alpha retained statics during an actual selective reload. Stop timeout ownership is implemented fail closed, but a busy-analysis timeout was not forced in these tests.

Six corrected observations:

- [Mono fixed](evidence/2026-10-08T09-48-09-543Z/observation.json)
- [Mono original](evidence/2026-10-08T09-49-22-913Z/observation.json)
- [Mono isolated stale result](evidence/2026-10-08T09-50-15-841Z/observation.json)
- [CoreCLR fixed](evidence/2026-10-08T09-51-13-200Z/observation.json)
- [CoreCLR original](evidence/2026-10-08T09-52-17-690Z/observation.json)
- [CoreCLR isolated stale result](evidence/2026-10-08T09-53-19-428Z/observation.json)

The runner creates an owned temporary project. It verifies exact executable metadata, uses local packages from the selected Editor and the toolkit sample cache, and checks the reviewed toolkit `VENDOR.json` hashes before copying four Roslyn DLLs and the Windows native DLL. That manifest identifies source commit `7caf244b213b62d95d2d76ad480d22c31778994b`; this proof adds no vendor attestation. Native rendering and PTY sessions are not opened.

The local fixture packages are Test Framework 1.6.0/NUnit 2.0.5 on 6.3, Test Framework 1.9.0/NUnit 2.1.2 on 7, and the existing sample's Newtonsoft 3.2.2. These fixture-only overrides do not change package dependencies or install registry packages.

The initial minimal 6.3 graph failed both direct and worker semantic calls because the reviewed Roslyn bundle could not load `System.Runtime.CompilerServices.Unsafe, Version=6.0.0.0`. A matching ReadyToRun DLL from the selected Editor's ILPP compiler was rejected by Mono with `Invalid data directory 3`. The corrected 6.3 fixture uses the selected Editor's IL-only `Data/NetStandard/EditorExtensions/System.Runtime.CompilerServices.Unsafe.dll`, with identity/hash recorded in each observation. Its SHA-256 is `01748200f2400c742aa689f1f5101bd6298efdfd92c00c18f4fa473847235ba9`. Unity 7 needs no copied Mono dependency. Upstream Roslyn distribution dependency closure remains unverified; the lifecycle pass does not certify that packaging.

Earlier failed observations are retained. The first test compile lacked an explicit existing Newtonsoft reference; the test asmdef now names Newtonsoft and NUnit. One fixture diagnostic attempt emitted a malformed C# newline and was corrected. Readiness polling was changed to observe the existing result sequence and consume once, but repeating that experiment still failed: polling did not cause the missing dependency. Fixture-only catch diagnostics expose the engine's swallowed exception without changing its null fallback; no production Roslyn catch was changed.

Reproduce from the fork root, with exact Editor and toolkit paths supplied by the operator:

```powershell
node experiments/coreclr-lifecycle-proof/run-proof.js --editor '<EDITOR_EXE>' --editor-version 6000.3.9f1 --toolkit '<TOOLKIT_ROOT>' --variant fixed
```

Repeat with `original` and `unread-result`, then the exact `7000.0.0a7` Editor. `--prepare-only` builds the disposable fixture without launching Unity. The runner bounds startup and execution, limits forced cleanup to the owned process tree on failure, and persists only sanitized structured results. Failed temporary fixtures retain raw XML/logs for private diagnosis; successful fixtures are removed after process absence is verified.
