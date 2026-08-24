# Lift roadmap

Lift is migrating from the inherited actor/reactor/layout architecture to one
deterministic core with explicit inputs and effects. The governing design is
[docs/design/2026-08-25-lift-core-architecture-design.md](docs/design/2026-08-25-lift-core-architecture-design.md).

The migration order is:

1. Lift naming and platform-independent core contracts.
2. Workspace catalog and grouped BSP model in shadow mode.
3. Immutable snapshot read paths for CLI, IPC, menu, Mission Control, and Stack
   Line.
4. Window lifecycle, floating/fullscreen state, and application rules.
5. Workspace commands, layout planning, focus, and frame effects.
6. Display/native Space topology, sleep/wake, and login-screen transitions.
7. Drag, pointer, gesture, haptic, animation, and Mission Control interactions.
8. Removal of the legacy reactor/layout/workspace state and obsolete commands.

Each stage must leave a buildable, testable application and must have exactly
one production writer for each migrated capability.
