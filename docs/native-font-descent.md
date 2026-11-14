# Canonical native font descent

Rune's layout FontMetrics uses nonnegative distances above and below baseline.
GPUI 0.2.2's Linux CosmicText backend supplies a negative signed descender:
its font_metrics implementation assigns `descent: -metrics.descent`. On the real
headless Linux TextSystem, DejaVu Sans Mono at 17px measured ascent 15.779785,
raw descent -4.0092773 and equal 0/i/m advances 10.234863. Rune previously rejected
that valid baseline before native startup could proceed.

Normalize the finite signed descender to its magnitude at the NativeFont measure
boundary. Positive-distance backends are unchanged. NaN/infinity remain errors;
the existing complete geometry/monospacing validator still runs afterward. This
changes no GPUI/vendor code, font lookup, public display protocol or Lisp state.

The official Application::headless diagnostic subsequently measured four
DISTINCT real font IDs (normal3, bold0, italic2, bold-italic1), identical ASCII
advances and compatible vertical metrics, and exited successfully. It opens no
Window and establishes no GPU paint, shaping, font fallback, Unicode-wide advance
or Doom/Org workflow parity. The larger font-bank/probe prerequisite remains a
separate patch; normal font measurement needs this correction independently.
