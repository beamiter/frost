# Native font-role scaling

The reconstructed shell used fixed role sizes even after Chrome font settings
changed. Native Iced paragraph measurements confirmed the defect: Chrome 14
and 18.2 both produced a 12px header (199.189 × 15.6 shaped bounds), an 8px
MODEL label, and an unchanged 12px inspector title.

## Chrome group

Chrome role sizes now use `role × effective_chrome_font_size / 14`. The base is
the existing Config default, not a new 10px setting. Topbar, navigation, header,
inspector and the composer action row each own their Chrome wheel region. The
native input/IME/PTY surface remains in the Content group. The inspector's
proportional labels and inset output text use the same Chrome scale; light rich
conversation components are unchanged in this batch.

Larger Chrome fonts keep the model selector on one line with its complete
tooltip. Unavailable Attach/Image/Code controls move into the existing Tools
menu when the row needs space; their disabled menu entries remain visible.
Current-step strings and queue titles have bounded single-line surfaces with
full-value tooltips. Only larger fonts increase the required row heights; the
navigation retains its own scrollable region and fixed bottom status.

The old native QA helper explicitly set Chrome to 10, which the fixed-size bug
silently ignored. It now sets Chrome to 14 to match the existing production
default and preserve the reference roles. This is a correction to the fixture
parameters. It does not change the application's default setting or migrate
stored user preferences. That Chrome-only batch kept Main at 12; those earlier
captures retain their original parameters.

## Evidence and measurement

`JTERM_NATIVE_FONT_PROBE` works only with the explicitly labelled reference
fixture. The observer reads the actual Iced Text paragraph after native
renderer layout: paragraph size, shaped bounds and glyph count. It accepts only
an exact whitelist of fixed sample strings, never arbitrary user, task, path
or terminal content. Normal operation returns the original Element without
the observer wrapper or file output. The observer does not create a renderer,
substitute measured text, add a timer, or request another redraw.

Before/after native runs and build manifests are under
`evidence/frost/font-followup` in the task workspace. Chrome 18.2 now produces a
15.6px header (258.946 × 20.28) and a 10.4px MODEL label. The rich headline stays
20px with 557.871 × 26 bounds, establishing the Content negative control.
Native Ctrl+wheel checks separately exercise all five Chrome regions and
verify the actual paragraph changes while the same shell/TTY and PTY grid
remain unchanged. Static screenshots do not stand in for those checks.

At Chrome 14, the full default header, navigation, inspector, central fixture
and topbar pixels match the before-fix capture. One footer paint texel varies
slightly between captures and also varies within the before binary; it is
reported rather than hidden or called a zero-pixel image difference. Default
role paragraph sizes and shaped extents are unchanged.

The Chrome batch passes strict debug all-targets/all-features Clippy and the
full 1959-pass/0-fail/3-ignored debug bin suite. The ordinary Block native QA
and production companion unique/hide/clear-return checks also pass. A bounded
font-only observer verifies all five Chrome wheel regions against actual
paragraphs and the same PID/TTY/grid; it uses short synthetic shell queries
and a failure-only ERR trap that did not execute in that successful run.

The ordinary Chat long-input gate remains open: a non-DEBUG run timed out at
the heredoc, while its DEBUG run passed with an exact four-line payload.
Frozen ffba runs with both Chrome 10 and 14 also passed the new script, so those
parameters alone do not establish a cause. Separate long-query font observers
also failed. A single `stty` line without the preceding identity line may be
consistent with a missing character in a repeated redirection operator, but
this is an unproven hypothesis. Failed attempts are retained; neither the
short-query font result nor static glyph measurements close that input gate.
No heredoc assertion, input delay, or production input ownership was changed
to make it pass.

## Content group

Rich heading, body, metadata, task, diff and gutter roles now use the same
ratio against the existing production Main default of 14. The new QA fixture
explicitly sets Main 14 as well as Chrome 14. Its former Main 12 parameter
was hidden by the fixed-role defect; it is not a new nominal font size or a
change to Config defaults. Main 14 preserves the authored 20px heading, 12px
body, 10px task/code and 9px chip/gutter roles.

Native renderer measurements before the Content change showed identical role
sizes at Main 14 and 18.2. After the change, Main 18.2 produces a 26px heading,
15.6px body, 13px task/code and 11.7px chip/gutter. The independent Chrome
header remains 12px and MODEL remains 8px. Larger user bubbles, task rows and
diff headers/rows retain enough height for the scaled text; the original
default geometry is preserved. Long code retains its unwrapped monospace
presentation and the Copy payload is unchanged.

At Main 14, the before/after topbar, left shell, center header, rich message
region and inspector have zero differing RGB pixels above the recorded
two-level threshold. Four footer text paint texels differ; their individual
RGB values and same-before-binary comparison are retained rather than calling
the complete image identical. Main 14's composer input starts near y609, four pixels above
the older Main 12 fixture; Main 18.2 starts near y599. These positions are
identical before and after this Content change, so they are not evidence of
an exactly 80px composer at every Main size. The empty-input/multiline budget
still needs its own input-state investigation.

Both Main sizes pass exact native diff Copy, unchanged settled reading pixels
across Tools open/close, and actual clipboard paste followed by shell
execution. A same-source Block fixture is also captured. This bounded smoke
does not replace the unresolved 2ms keyboard stream gate. A later accepted
Bash history capture proves that one 170-byte synthetic command arrived as
164 bytes, missing bytes 43..49 (` >> /h`). Short paired old/current runs
subsequently received all 170 bytes; read-only kernel size sampling stayed
34 rows by 94 columns. Those passes exclude neither intermittent loss nor
unobserved input stages. The event-to-PTY layer remains to be localized.

This document does not certify physical IME, every font/DPI combination, or
complete 1:1 reference fidelity. New screenshots are local evidence until the
parent task saves the selected final images; this batch does not publish code.
