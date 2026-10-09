# Frost native-key observer patch

Upstream: iced_winit 0.14.0, official crates.io archive. UPSTREAM_INTEGRITY.json records its Cargo.lock-verified archive SHA256 and all packaged file hashes. The archive omits the workspace license; LICENSE is copied from the exact upstream commit 3997291f318a8bc06fa522f5579836fb3feb94df.

Only src/lib.rs is modified; src/native_events.rs is added. An optional process-local callback observes window-scoped native events before conversion. Without registration the event path is unchanged. The callback can consume only real key presses; releases, focus events and synthetic snapshots always continue through the existing conversion path. No synthetic action is generated.

Frost uses positive synthetic Enter presses as held-key snapshots and ignores synthetic releases as release evidence. Actual physical releases end native held ownership. Frost separately acknowledges queued Iced Enter deliveries so a release processed before a widget message cannot erase delivery ownership. A captured prefix count owns only deliveries already pending at the claim; subsequently appended fresh presses do not inherit that count. Passive surface refresh does not claim queued confirmations. This prevents a key first pressed outside the window from submitting a command recalled by mouse immediately after focus returns.

On every Iced/winit upgrade, verify the recorded upstream delta, event placement before conversion, physical identities, focus snapshots, both X11/Wayland feature configurations, and the non-executing native recorder matrix. Remove the patch if an equivalent supported upstream hook becomes available. The local patch is limited to this native event boundary.

Native recorder proof covers X11. X11-only and Wayland-only builds are checked separately. Pinned Winit clears its Wayland repeat state and timer on keyboard Enter/Leave and starts repeats from delivered Key(Pressed) events; the missing focus snapshot alone therefore does not establish the same failure there. This is a source-based inference, not real Wayland runtime validation.
