(dolist (p '(
("(progn (defvar vm-top-a 1) (list (let ((vm-top-a 2)) (list (default-toplevel-value 'vm-top-a) (set-default-toplevel-value 'vm-top-a 3) vm-top-a)) vm-top-a))" "((1 nil 2) 3)")
("(progn (defvar vm-top-b nil) (makunbound 'vm-top-b) (list (let ((vm-top-b 1)) (list (condition-case nil (default-toplevel-value 'vm-top-b) (void-variable 'unbound)) (set-default-toplevel-value 'vm-top-b 2) vm-top-b)) vm-top-b))" "((unbound nil 1) 2)")
("(list (default-toplevel-value nil) (default-toplevel-value t) (default-toplevel-value :vm-top-key))" "(nil t :vm-top-key)")
("(progn (defvar vm-top-gc '(\"kept\")) (let ((vm-top-gc nil)) (set-default-toplevel-value 'vm-top-gc '(\"moved\")) (garbage-collect) (default-toplevel-value 'vm-top-gc)))" "(\"moved\")")
)) (unless (equal (eval (read (car p)) t) (read (cadr p))) (error "default mismatch %S" p))) (princ "default oracle passed\n")