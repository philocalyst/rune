# Active backtrace source candidate (unqualified)
Live owner-rooted activation snapshots preserve original function identity and forms/values. Interpreter specials/argument evaluation retain unevaluated entries; ordinary calls update same entry; native/funcall/optimized VM entries own scoped cleanup. Base lookup resolves aliases and handles direct-dispatch special identities; snapshot membership is fixed across callbacks and GC. Distinct funcall-interactively marker supplies real provenance. No getenv/marker shim.
Source-only:12 interpreter/native tests plus4VM tests; one differential test includes5 GNU expressions. No build/tests yet. Unsupported debug-on-exit mutation/thread-backtrace introspection not invented; general special-form function-cell conformance remains outside this slice.

V2 source-only compile corrections: rooted defun arguments for mutable Context, explicit IntoArray trait imports, bound Object for rooted iterator, explicit Object conversion in VM assertion. V1 failed build preserved, no tests executed. No acceptance assertions removed.

V3 compile correction: dedicated checked four-field active-entry parser because existing IntoArray only supports one/two; List iteration explicitly uses as_list. No semantic/assertion change; v2 failed build preserved.

V4 source-only correction explicitly types the frame parser closure as anyhow::Result and propagates missing-field and ConsError separately. V3 never compiled/run; tests and semantics unchanged.
