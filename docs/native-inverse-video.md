# Inverse-video color publication

The bounded owner face resolver supports nil, t and unspecified inverse-video
values on selected-frame default faces, named inherited faces and anonymous
buffer face plists. Unspecified inherits; explicit nil overrides inherited t.
After foreground/background and inversion are merged, colors swap once. A
foreground-following underline resolves against the final displayed foreground;
explicit underline colors remain unchanged.

Only plain final Face colors cross into renderer/SVG DTOs. Direct mutable face
vector/plist changes are re-resolved on each owner turn and compared exactly;
style-only publication does not change Session sequence or IME authority.
Nonboolean inverse values fail publication without preventing corrective Lisp.
Existing alias/list/visible-range/work limits apply unchanged.

This is not full GNU face merging, overlays/font-lock, graphical frame or native
window verification. Font families/weights/slants, decorations beyond the
existing single-line underline and full Org-mode styling remain outside scope.
