# Editing Rust

`Source` holds parsed Rust source. `Source::select` resolves a `Selector` to exactly one `Selected` object. An edit consumes the selection, and later queries see the edited source.

```rust
use r#override::{Edition, Result, Source, item};

fn main() -> Result<()> {
    let mut source = Source::parse(
        "struct State { value: u32 }",
        Edition::Edition2024,
    )?;
    source.select(item("State").field("value"))?.set_type("u64")?;
    println!("{source}");
    Ok(())
}
```

`Source::edition()` returns the edition used for parsing, and `Selected::text()` returns the selected text. `Source` implements `Display` for writing out the result.

The snippets below use a `Source` named `source` and the `item`, `call`, `arm` and `root` functions from `r#override`. Names like `State` and `run` refer to declarations in the code being edited.

## Selecting a target

Queries follow declarations and code structure:

```rust
item("Runtime::run")
    .match_expr()
    .has(arm("Command::Print"))
```

Each step searches within the results of the previous one. `item`, `call` and `arm` can also start a query as free functions, and `root()` starts from the current scope.

| Selection | Meaning |
| :--- | :--- |
| `item("State")`, `item("Runtime::run")` | A named declaration or qualified member |
| `implementation("Runtime")` | An implementation for a type |
| `call("dispatch")` | A function or method call |
| `macro_call("assert_eq")` | A macro invocation |
| `import("std::io::Write")` | An import path |
| `record("State")` | A record construction |
| `pattern("Command::Repeat")` | A named destructuring pattern |
| `arm("Command::Print")` | A match arm for a named pattern |
| `binding("message")` | A binding in a pattern |
| `for_loop()`, `while_loop()`, `loop_expr()` | A loop |
| `if_expr()`, `match_expr()`, `closure()` | A conditional, match or closure |

Parts of an object have their own steps:

```rust
item("State").field("value")
item("dispatch").parameter("command")
item("Command").variant("Repeat")
item("dispatch").generic("T")
item("dispatch").attribute("inline")
```

`attribute` takes an attribute path, such as `"inline"`, or the full attribute contents, such as `"cfg(feature = \"active\")"`. `body()` selects the body of a function, closure, loop or match arm. `condition()` selects an `if` or `while` condition, or a `for` iterable.

### Narrowing a query

`has(query)` keeps objects that contain a match. `child(query)` looks only at the nearest structural level instead of all descendants:

```rust
item("run").body().child(root().binding("message"))
```

`and(query)` and `not(query)` check whether the same object also matches another query. `or(query)` merges results without duplicates.

Other filters check known relationships:

```rust
item("Buffer::write").implementing("Write")
item("State").field("value").of_type("u32")
item("run").if_expr().references("self.ready")
```

`of_type` compares declared types using the available declarations. `references` checks whether the object mentions a path or field expression.

The query passed to `select` must match exactly one object; no match or several matches is an error. An enclosing declaration, trait, type or structural step can narrow an ambiguous query.

### Macro inputs

Queries also search macro inputs that parse as Rust items, statements or comma-separated expressions:

```rust
source
    .select(
        item("check")
            .macro_call("assert_eq")
            .call("calculate"),
    )?
    .redirect("patched::calculate")?;
```

In `assert_eq!(calculate(x), expected())`, this changes the first call and writes it back into the macro's tokens. Nested macros work the same way.

Queries see only the tokens, not what the macro does with them. `stringify!(calculate(x), expected())` also contains a selectable call, although the macro turns it into a string. The patch decides which selections make sense for a given macro.

## Associating declarations

Parameter and field names are matched using the visible declarations, module paths, imports, aliases, enclosing implementations and local bindings.

For source spread across files, set the current module and add the other files as context:

```rust
source.set_module("runtime::worker")?;
source.add_source("runtime::shared", &shared);
```

`add_source` only provides declarations for lookup; edits still apply to `source` alone.

### Named call arguments

`argument("name")` uses the called function's declaration to find the argument for that parameter:

```rust
source
    .select(item("run").call("dispatch").argument("on_ready").closure())?
    .delegate("decorate", &["context"])?;
```

For a function outside the available source, supply its declaration directly:

```rust
use r#override::Declaration;

let dispatch = Declaration::parse(
    "fn dispatch(on_error: ErrorHandler, on_ready: ReadyHandler)",
    source.edition(),
)?;

source
    .select(
        item("run")
            .call("dispatch")
            .declared_by(&dispatch)
            .argument("on_ready")
            .closure(),
    )?
    .delegate("decorate", &["context"])?;
```

`Source::declaration(query)` and `Selected::declaration()` take a declaration from parsed source instead. `declared_by` attaches such a declaration to a call or symbol query.

`argument("self")` selects a method receiver. Other named arguments can be changed with `set_value` or removed with `remove`.

### Interface and layout descriptions

`describe` adds declarations that the source does not contain:

```rust
source.describe("type Outcome<T> = std::result::Result<T, Error>;")?;
```

It also accepts record and tuple-struct patterns with named bindings, which give names to positional fields:

```rust
source.describe("Pair(left, right)")?;
source.select(item("Pair").field("right"))?.set_type("u16")?;
```

For `struct Pair(u8, u8);`, the pattern supplies the field names used by the query. Destructuring patterns and getters in the available source can supply them as well.

Descriptions are taken as written; the compiler checks the resulting types and borrows.

## Editing declarations and values

Each editing method takes the Rust fragment it inserts or replaces:

```rust
source.select(item("State"))?.add_field("pub(crate) shared: Shared")?;
source.select(item("State").field("value"))?.set_type("u64")?;
source.select(item("run"))?.set_visibility("pub(crate)")?;
source.select(item("run"))?.add_attribute("#[inline]")?;
```

| Operation | Input and effect |
| :--- | :--- |
| `rename("name")` | Changes a declaration name or import alias |
| `set_binding("mut value")` | Changes a selected named binding's pattern, including its `mut`, `ref` or `@` modifiers |
| `set_visibility("pub(crate)")` | Changes declaration visibility; `""` selects private visibility |
| `add_attribute("#[inline]")` | Adds an attribute before the declaration header |
| `add_field("field: Type")` | Adds a declaration field; on a record expression, accepts an initializer such as `"field: value"` |
| `add_variant("Repeat { count: usize }")` | Adds an enum variant |
| `add_parameter("context: &Context")` | Adds a function or closure parameter |
| `add_generic("T: Send")` | Adds a generic parameter |
| `set_bounds("Send + Sync")` | Sets bounds on a parameter, trait or type alias |
| `set_where_clause("where T: Clone")` | Sets the where clause; `""` removes it |
| `set_type("u64")` | Replaces a selected type or an object's declared type |
| `set_return_type("Result<()>")` | Changes a function's return type |
| `set_signature("pub fn run(input: &Input)")` | Replaces a function header, retaining its body and attributes |
| `remove()` | Removes a declaration, member, attribute, import or named argument |

`rename` changes only the declaration or alias. [Symbol redirection](#redirecting-symbols-and-calls) updates the references to it.

Values and conditions take expressions:

```rust
source
    .select(item("make_state").record("State").field("value"))?
    .set_value("compute_value()")?;
source.select(item("LIMIT"))?.set_value("64")?;
source.select(item("run").while_loop())?.set_condition("self.ready()")?;
```

`set_value` works on record-field initializers, constants, statics, enum discriminants and named call arguments. `set_condition` replaces the condition of an `if` or `while`, or the iterable of a `for`.

To let a destructuring pattern accept other fields, add `..`:

```rust
source.select(item("run").pattern("State"))?.add_rest()?;
```

### Adding members and choosing their position

New items go where Rust's ordering expects them, such as after a receiver, after lifetime parameters or before a trailing `..`. `at` chooses a specific position:

```rust
source
    .select(item("dispatch").match_expr().has(arm("Command::Print")))?
    .at(arm("Command::Quit").before())
    .add_arm("Command::Repeat { message, count } => repeat(message, count)")?;
```

`at` also applies to attributes, fields, variants, parameters, generics, call arguments, imports and mounted modules. Its query searches within the selected object.

A new match arm goes before a trailing catch-all by default; `at` places it elsewhere.

```rust
source.select(item("run").call("dispatch"))?.add_argument("context")?;
source.select(root())?.add_use("use crate::support::Context;")?;
source.select(root())?.mount_module("mod support", &support_path)?;
```

`mount_module` adds a module declaration with a `#[path = ...]` attribute; `"pub mod support"` makes it public. New imports join the existing ones at the top, and items added to a block go before its final expression.

## Redirecting symbols and calls

`redirect` points a call, macro or import at a new target:

```rust
source
    .select(item("run").call("calculate"))?
    .redirect("crate::support::calculate")?;
```

Generic arguments on the call are kept unless the new target supplies its own. A method call can be redirected to another method name; to pass the receiver to a free function, use [delegation](#delegating-through-a-helper).

To update every reference to a declaration in a scope, end the query with `symbol`:

```rust
source.select(root().symbol("State"))?.redirect("crate::shared::State")?;
```

The name is resolved through the scope's declarations and imports, and every path and associated reference to that declaration is rewritten. When the name is ambiguous, `declared_by` identifies the declaration.

A symbol selection covers the references and supports `redirect`. A declaration selection covers the declaration itself and supports edits such as `rename`, `set_visibility` and `remove`.

## Reusing upstream logic

### Delegating through a helper

Delegation routes a call through a helper:

```rust
source
    .select(item("Runtime::run").call("block_on"))?
    .delegate("crate::support::block_on", &["context"])?;
```

The helper receives the context arguments first, then the original receiver if any, then the original arguments. Explicit generic arguments are passed to the helper.

For a closure, the helper receives the closure after the context. A delegated function or region is first wrapped in a closure; async code is wrapped in an async block instead, and the helper call is awaited.

The helper then decides how to run the original code:

```rust
source.select(item("run"))?.delegate("instrument", &["context"])?;
```

A delegated region may only exit in ways that work inside a closure or async block. For returns or breaks out of the surrounding code, use extraction, described below.

### Selecting a region

Whole functions, loops, match arms and closure bodies can be extracted directly. For a run of statements, mark its start and end as a region:

```rust
let region = item("run").body().region(
    root().binding("name").before(),
    call("report").before(),
);
```

`before()` and `after()` refer to the edges of the selected object, including a statement's semicolon; for a binding, the edges of its `let` statement. `start()` and `end()` refer to the edges of the object's body or contents.

Both boundary queries search the current scope and must each match one object. The region must cover complete syntax and form a valid Rust body for extraction or delegation.

A region depends on code order: code added upstream between its boundaries becomes part of it. Selecting a whole function, loop or branch avoids this when one already covers the reused code.

### Extracting a function

```rust
source
    .select(item("Runtime::run").arm("Mode::Active"))?
    .extract(
        "fn run_active(&mut self, input: &Input) -> usize",
        &["input"],
    )?;
```

The signature defines the new helper, and `arguments` gives the expressions passed at the call site. A `self` receiver makes it a method call, so `arguments` covers only the remaining parameters.

The helper goes next to the original function, in the same `impl` or trait if there is one. An async signature makes the call awaited.

The extracted code runs in its own function, so moves, borrows and drop order follow from the chosen signature, arguments and outputs.

### Carrying control flow

Options go before `extract`:

```rust
source
    .select(item("Parser::parse").for_loop().body())?
    .control_flow()
    .propagate()
    .extract(
        "fn parse_item(&self, text: &str, output: &mut Vec<i32>)",
        &["text", "output"],
    )?;
```

`control_flow()` handles `return`, `break` and `continue` that leave the selection: the helper reports the exit, and the call site performs it with the original label and value. Exits that stay within a loop or closure inside the selection are unchanged.

A single kind of exit uses `core::ops::ControlFlow` with its value directly. Several kinds use a generated enum named after the helper, such as `ParseItemExit`, with the helper's visibility.

`propagate()` keeps `?` working: the helper returns the enclosing `Result`, `Option` or `ControlFlow` type, and the call site applies `?`. Aliases and supplied declarations help determine that type.

The signature's return type is the helper's normal result. With both options in a function returning `Result`, a normal result `Output` becomes something like `Result<ControlFlow<Exit, Output>, Error>`. Exit value types come from the function's return type, the loop's type or the value's declaration.

### Returning local bindings

`outputs` lists variables declared in the extracted code that the rest of the function still uses:

```rust
fn prepare(raw: &str) -> usize {
    let name = raw.trim().to_owned();
    let mut count = name.len();
    count += 1;
    report(&name, count);
    count += 1;
    count
}
```

Extract the declarations and the first increment, everything before `report`:

```rust
source
    .select(item("prepare").body().region(
        root().start(),
        call("report").before(),
    ))?
    .outputs(["name", "mut count"])
    .extract(
        "fn parse_name(raw: &str) -> (String, usize)",
        &["raw"],
    )?;
```

The call site receives them as:

```rust
let (name, mut count) = parse_name(raw);
```

Each entry is a variable name with an optional `mut`. It refers to the variable visible at the end of the region, which must be declared within the selection. `mut` is explicit because the variable may change in the helper, after the call, or both.

The helper's own variables can be edited with the same selectors. For example, extracting only `let mut count = 1;` into `seed` leaves all changes after the call, so the helper's variable no longer needs `mut`:

```rust
source.select(item("seed").binding("count"))?.set_binding("count")?;
```

One output is returned directly; several are returned as a tuple in the listed order. If the selected code also produces an expression value, that value comes first and replaces the original expression.

The signature's return type covers this whole result, wrapped by `control_flow()` and `propagate()` as above. Outputs are returned only on normal completion; early exits follow their control-flow path.

Variables declared before the selection come in through parameters and arguments; listing one in `outputs` is an error.

## Runnable example

[examples/edit.rs](../examples/edit.rs) adds a repeat command to a command dispatcher. It extracts the print arm's code into a helper, adds an enum variant and makes the new arm call the helper.

```sh
cargo run --example edit
```

The example prints the edited program. Its `main` takes a message and a repeat count.
