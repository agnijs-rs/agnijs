# Security policy

AgniJS is a JavaScript engine meant to run code its embedder does not trust, so a bug that lets a script escape what the embedder granted is a security bug. This page says how to report one and what happens next. What the engine promises, and what it leaves to the embedder, is in [docs/security-model.md](docs/security-model.md).

## Reporting a vulnerability

**Do not open a public issue, pull request or discussion for a vulnerability.** Report it privately with GitHub's private vulnerability reporting: "Report a vulnerability" on the repository's [Security tab](https://github.com/agnijs-rs/agnijs/security). That is the only channel; there is no security email address.

Include what you can of:

- the AgniJS commit you tested;
- the embedding: the Rust API, the C API or the `agnijs` CLI, the cargo features (`jit`, `intl`, `linear-regex`) and any limits set (memory, fuel, interrupt, stack);
- the smallest script or input that shows the problem, and what happens (crash, wrong result, hang, capability reached);
- which guarantee in docs/security-model.md it breaks, if you know.

A failing input from the fuzzers (`target/fuzz-inputs/` or `fuzz/artifacts/`, see [docs/fuzzing.md](docs/fuzzing.md)) is a good report on its own.

## Supported versions

AgniJS is pre-release. No version has been published to crates.io and there are no release branches. Only the `main` branch is supported: fixes land there, and nothing is backported. This section will list release lines once there are any.

| Version | Supported |
| --- | --- |
| `main` | Yes |
| Any other commit or fork | No |

## What to expect

AgniJS is provided as is, with no warranty and no support commitment, so no promises can be made about how quickly a report is answered or a vulnerability fixed. Security is nonetheless very important to this project, and every report will be acted on as soon as possible.

What a report leads to:

- an assessment: whether it is in scope, and how severe;
- a fix on `main`, with a regression test, and a GitHub security advisory once the fix is public. Reporters are credited unless they ask not to be.

Please keep the issue private until the fix is public, or until we have agreed a disclosure date with you. If it is being exploited, say so.

## Scope

In scope is anything that breaks a guarantee in [docs/security-model.md](docs/security-model.md#guarantees) when the attacker supplies JavaScript source and the data it works on, for example:

- memory unsafety or a crash of the host process from a script, including through the C API or the JIT;
- a panic that escapes the C API, which promises never to unwind into C;
- a script reaching a host capability the embedder did not grant;
- a configured limit (memory, fuel, interrupt, native stack, string length) that a script can get around, or a path that blocks the host thread without ever polling the interrupt;
- a pattern eligible for the linear matcher that still runs in superlinear time under the `Auto` or `LinearWhenEligible` policy (patterns with backreferences or lookaround are [not covered](docs/security-model.md#regular-expressions-redos));
- the CDP server accepting a request from a web page (a DNS-rebinding host, a foreign `Origin`), which it promises to refuse.

Out of scope, because the security model already treats them as trusted or does not promise them (each links to the reason):

- bytecode or snapshot bytes written by the attacker ([trusted input](docs/security-model.md#bytecode-and-snapshots-are-trusted-input)): the checksum is not a signature;
- anything done by a client already connected to the debugger or the CDP endpoint, which has [full control](docs/security-model.md#the-debugger-and-the-cdp-endpoint-are-full-control);
- [side channels](docs/security-model.md#side-channels) such as timing and Spectre, and isolation between realms of one engine ([not a boundary](docs/security-model.md#realms-are-not-a-security-boundary));
- bugs in the embedder's own host functions or `Host` hooks ([host bugs](docs/security-model.md#host-bugs));
- a panic in the Rust API that leaves the engine [poisoned](docs/security-model.md#panics-in-the-rust-api). We still want to hear about panics, but report them as ordinary issues: the engine is meant never to panic, and a panic is a bug, not a vulnerability, unless it also breaks a guarantee above.

If you are not sure, report it privately anyway.
