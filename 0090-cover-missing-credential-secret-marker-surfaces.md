---
id: 90
created: 2026-11-11
depends-on: []
---

# Cover missing credential secret-marker surfaces

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

The HTTPS fixture already scans the cdenv root and repository for synthetic tokens, checks stale-token diagnostics, and verifies native-helper isolation plus byte-preserved Git configuration. Component tests already cover redacted representations. The outstanding gap is direct observation of the remaining prohibited surfaces, including failure/revocation paths.

## Done when

- Use unique synthetic credential payload/request markers and verify intended private recipients actually receive them, preventing an unused-marker false pass.
- Observe active cdenv/helper process arguments and environment, managed container snapshots/configuration/native-helper storage, and captured debug/error/status output. Assert credential payload markers are absent from those prohibited surfaces as well as managed host persistence.
- Cover success and deterministic failure/revocation paths; exclude intentional private fixture stores/recipients explicitly and never retain personal credentials in artifacts.
- Add named tests and the exact inspected surfaces to the secret-marker traceability row, reusing existing persistence/native-helper checks instead of recreating them. Run the affected tests.

This is a bounded marker-coverage task, not an open-ended audit of every security property. Existing core tests already cover malformed/truncated/oversized frames, scoped identity/revision mismatch and admission limits.
