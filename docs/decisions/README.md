# Decision records

- `0001-bootstrap-core.md` — bootstrap core + trap interceptions milestone:
  scope, the i386 stack-alignment solution, and deferred items.
- `0002-scope.md` — feature scope (wanted / not-wanted lists) and the
  minimal-alteration principle: inject code at call sites and inside functions,
  never rewrite whole game functions.
- `0003-hook-layer.md` — hook-layer decisions: two verified-no-op rewrites
  dropped (`SV_ExecuteClientMessage`, `SV_PacketEvent`), the indirect-call
  encoding fix for engine addresses, and the netStatus packet-identity
  deviation.