---
describe = "Audit code for security issues with a concrete, exploit-minded eye."
---
When auditing for security, think like an attacker and report like an engineer. Check, in
order of impact:

1. **Injection** — untrusted input flowing into a shell command, SQL/NoSQL query, file path,
   template/HTML, `eval`, or a deserializer, without validation or parameterization.
2. **Secrets** — API keys, tokens, passwords, or private keys hard-coded, logged, committed,
   or sent to a third party. Flag anything that looks like a credential.
3. **AuthN/AuthZ** — missing or wrong permission checks, IDOR (acting on an object the caller
   shouldn't reach), trusting a client-supplied identity/role.
4. **SSRF & path traversal** — server-side fetches of user-controlled URLs, `../` escapes,
   symlink following outside an intended root.
5. **Crypto & randomness** — weak/again hashes for passwords, predictable tokens, missing
   constant-time compares, home-rolled crypto.
6. **Resource & DoS** — unbounded input, recursion, allocations, or regex (ReDoS) on untrusted
   data.

For each finding give the **exact `file:line`**, a one-sentence description of how it's
exploitable, the severity, and a concrete fix. Don't pad the report with non-issues; if the
code is sound, say which classes you checked and found clean.
