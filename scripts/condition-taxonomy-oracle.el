;; Exact metadata-driven matching qualification, independent of Rune defaults.
(dolist (case '(((condition-case e (signal 'rune-unknown nil) (rune-unknown 'self) (error 'err) (t 'all)) all)
                ((progn (put 'rune-child 'error-conditions '(rune-child error)) (condition-case e (signal 'rune-child nil) (error 'err) (t 'all))) err)
                ((condition-case e (signal 'quit '(x)) (error 'wrong) (quit e)) (quit x))
                ((let ((inhibit-quit t)) (condition-case e (signal 'quit nil) (t e))) (quit))))
  (unless (equal (eval (car case) t) (cadr case)) (error "oracle mismatch %S" case)))
(princ "4 taxonomy cases passed\n")
