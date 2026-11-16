# Code

```
plain fence with <b>markup</b> &amp; an entity
	a tab-indented line
trailing spaces follow   
```

```rust
// A comment with <angle> brackets & an ampersand.
fn main() {
    let message = "quoted \"text\" here";
    println!("{message}");
}
```

~~~python
def greet(name):
    return f"hello {name}"  # comment
~~~

```diff
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,4 @@ fn context()
 unchanged line
-removed <line> & more
+added "line" with café
\ No newline at end of file
```

```patch
-old
+new
```

Inline `code` after the blocks, and a fence inside a list:

- item

  ```udiff
  -inside a list
  +still a diff
  ```
