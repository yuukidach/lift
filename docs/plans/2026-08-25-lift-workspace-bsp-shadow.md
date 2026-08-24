# Lift workspace and BSP shadow migration

## Boundary

The legacy `LayoutEngine` remains the only production writer. The new core may
validate and compare workspace state, but it emits no effects and cannot move,
focus, resize, or hide a real window.

## Steps

1. Implement the pure `WorkspaceCatalog` and grouped BSP arena with invariant
   checks after every tested operation.
2. Add a stable, read-only legacy projection containing display bindings,
   active/last workspace numbers, tiled groups, floating membership, and window
   assignment.
3. Normalize that projection into the core shadow model after legacy mutations
   and compare the resulting snapshot. Report a compact structured diff; never
   repair production state from the shadow.
4. Cover startup, workspace switching, moving windows, floating transitions,
   join/unjoin, and multi-display workspace ownership with scenario tests.

## Exit condition

The all-targets suite is green, core invariants hold for every scenario, and
stable comparison points produce no shadow differences. Temporary differences
inside a legacy multi-owner transaction are recorded as migration evidence and
must never fail or repair the running window manager. Cutover is explicitly out
of scope for this stage.
