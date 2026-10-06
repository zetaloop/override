# Editing Rust

`Source` holds parsed Rust source. A `Selector` describes an object, and `Source::select` resolves it to one `Selected` object. An editing operation consumes that selection; the next query observes the edited source.

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

`Source::edition()` returns the parsing edition. `Selected::text()` returns the selected text, and `Source` implements `Display` for writing the result.

The following snippets use a `Source` named `source` and the `item`, `call`, `arm` and `root` constructors exported by `r#override`. Declaration names refer to the code being edited.

## Selecting a target

Queries follow declarations and structure:

```rust
item("Runtime::run")
    .match_expr()
    .has(arm("Command::Print"))
```

Each step searches within the objects produced by the previous step. `item`, `call` and `arm` also have free-function forms that start a query; `root()` starts at the current selection scope.

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

Parts of a selected object have their own queries:

```rust
item("State").field("value")
item("dispatch").parameter("command")
item("Command").variant("Repeat")
item("dispatch").generic("T")
item("dispatch").attribute("inline")
```

`attribute` accepts an attribute path, such as `"inline"`, or complete attribute contents, such as `"cfg(feature = \"active\")"`. `body()` selects a function, closure, loop or match-arm body. `condition()` selects an `if` or `while` condition, or a `for` iterable.

### Narrowing a query

`has(query)` retains objects containing a match. `child(query)` follows the nearest structural child rather than every descendant:

```rust
item("run").body().child(root().binding("message"))
```

`and(query)` and `not(query)` test whether the same object matches another query. `or(query)` combines results and removes duplicates.

Additional filters express known relationships:

```rust
item("Buffer::write").implementing("Write")
item("State").field("value").of_type("u32")
item("run").if_expr().references("self.ready")
```

`of_type` compares declared types using the available declaration context. `references` looks for a named path or field expression in the selected object's syntax.

A completed selection must identify one object. Missing and ambiguous results produce errors. An enclosing declaration, trait, type or structural relationship can provide the additional information needed to identify the intended object.

### Macro inputs

Queries traverse macro inputs that parse as Rust items, statements or comma-separated expressions:

```rust
source
    .select(
        item("check")
            .macro_call("assert_eq")
            .call("calculate"),
    )?
    .redirect("patched::calculate")?;
```

For `assert_eq!(calculate(x), expected())`, this changes the first call and writes it back through the surrounding token tree. Nested macros use the same mechanism.

These are views of the input syntax. `stringify!(calculate(x), expected())` also contains a selectable call-shaped fragment, although the macro turns those tokens into a string. The patch chooses the interpretation appropriate to the macro being edited.

## Associating declarations

Parameter and field names can come from the available declarations, module paths, explicit imports, aliases, enclosing implementations and local bindings.

For source spread across files, name the current module and provide the other source:

```rust
source.set_module("runtime::worker")?;
source.add_source("runtime::shared", &shared);
```

`add_source` supplies declaration context. Edits apply to `source`; the supplied `shared` source remains its own object.

### Named call arguments

`argument("name")` follows the call's declaration to locate the corresponding argument:

```rust
source
    .select(item("run").call("dispatch").argument("on_ready").closure())?
    .delegate("decorate", &["context"])?;
```

An interface declaration can supply parameter names for an external call:

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

`Source::declaration(query)` and `Selected::declaration()` obtain declaration objects from parsed source. `declared_by` associates a call or symbol query with such an object.

`argument("self")` identifies a method receiver. Ordinary named arguments can be edited with `set_value` or `remove`.

### Interface and layout descriptions

`describe` accepts Rust interface declarations:

```rust
source.describe("type Outcome<T> = std::result::Result<T, Error>;")?;
```

It also accepts named record and tuple-struct binding patterns. These express layout knowledge for fields that have positional Rust syntax:

```rust
source.describe("Pair(left, right)")?;
source.select(item("Pair").field("right"))?.set_type("u16")?;
```

For an input such as `struct Pair(u8, u8);`, the pattern supplies the field names used by the query. Destructuring patterns and getter field accesses in the available source can also supply these relationships.

Descriptions express the patch's interface knowledge. Rust compilation checks the resulting types and borrows.

## Editing declarations and values

Editing methods accept the corresponding Rust fragment:

```rust
source.select(item("State"))?.add_field("pub(crate) shared: Shared")?;
source.select(item("State").field("value"))?.set_type("u64")?;
source.select(item("run"))?.set_visibility("pub(crate)")?;
source.select(item("run"))?.add_attribute("#[inline]")?;
```

| Operation | Input and effect |
| :--- | :--- |
| `rename("name")` | Changes a declaration name or import alias |
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

`rename` operates on the declaration or alias itself. [Symbol redirection](#redirecting-symbols-and-calls) follows references associated with a declaration.

Named value slots and conditions use expression fragments:

```rust
source
    .select(item("make_state").record("State").field("value"))?
    .set_value("compute_value()")?;
source.select(item("LIMIT"))?.set_value("64")?;
source.select(item("run").while_loop())?.set_condition("self.ready()")?;
```

`set_value` accepts record-field initializers, constants, statics, enum discriminants and named call arguments. `set_condition` updates an `if` or `while` condition, or a `for` iterable.

For destructuring that should accept additional record fields:

```rust
source.select(item("run").pattern("State"))?.add_rest()?;
```

### Adding members and choosing their position

List additions follow the selected list's layout. Receivers precede ordinary parameters, lifetime parameters precede type and const parameters, and fields precede a trailing `..`. A specific insertion point is expressed with `at`:

```rust
source
    .select(item("dispatch").match_expr().has(arm("Command::Print")))?
    .at(arm("Command::Quit").before())
    .add_arm("Command::Repeat { message, count } => repeat(message, count)")?;
```

The same positioning option applies to attributes, fields, variants, parameters, generics, call arguments, imports and mounted modules. Its boundary query is resolved within the selected owner.

Match-arm insertion places a specific arm before a trailing catch-all by default. `at` expresses an explicit position when the order matters.

```rust
source.select(item("run").call("dispatch"))?.add_argument("context")?;
source.select(root())?.add_use("use crate::support::Context;")?;
source.select(root())?.mount_module("mod support", &support_path)?;
```

`mount_module` creates a module declaration with a `#[path = ...]` attribute. A complete declaration such as `"pub mod support"` controls visibility. Imports join the leading declaration group; items inserted into a block precede its tail expression.

## Redirecting symbols and calls

`redirect` changes the target of a selected call, macro or import:

```rust
source
    .select(item("run").call("calculate"))?
    .redirect("crate::support::calculate")?;
```

Call redirection carries existing generic arguments unless the supplied target provides its own. A method can be redirected to another method name; [delegation](#delegating-through-a-helper) passes its receiver to a qualified helper function.

To follow a declaration's references throughout a scope, finish the query with `symbol`:

```rust
source.select(root().symbol("State"))?.redirect("crate::shared::State")?;
```

The symbol is resolved using the scope's declarations and imports. Qualified paths and associated references are then rewritten for that declaration. `declared_by` can identify the declaration when several namespaces or external interfaces are involved.

A symbol selection represents references and supports redirection. A declaration selection represents the named object and supports edits such as `rename`, `set_visibility` and `remove`.

## Reusing upstream logic

### Delegating through a helper

Delegation routes a call through a helper:

```rust
source
    .select(item("Runtime::run").call("block_on"))?
    .delegate("crate::support::block_on", &["context"])?;
```

The helper receives the supplied context first, then the original method receiver, if present, and the original arguments. Explicit call generics travel to the helper.

For a closure, the helper receives the original closure after the context. For a whole function or region, delegation wraps the selected code in a closure. Async code uses an async block and awaits the helper.

This lets the helper decide how to invoke the upstream operation:

```rust
source.select(item("run"))?.delegate("instrument", &["context"])?;
```

Region delegation requires exits to fit the generated closure or async block. Function extraction provides the explicit control-flow conversion described below.

### Selecting a region

Functions, loops, match arms and closure bodies provide structural extraction targets. A region joins two named boundaries when the desired operation spans several statements:

```rust
let region = item("run").body().region(
    root().binding("name").before(),
    call("report").before(),
);
```

`before()` and `after()` refer to the selected object, including a statement's semicolon. For a binding they refer to its enclosing declaration. `start()` and `end()` refer to the selected object's body or contents.

The two boundary queries run within the current scope and must each identify one object. Their interval must contain complete syntax elements. The selected contents must form a Rust body for extraction or delegation.

Regions depend on source order: upstream code added between the anchors becomes part of the region. A whole function, loop or branch provides an alternative when it already expresses the operation being reused.

### Extracting a function

```rust
source
    .select(item("Runtime::run").arm("Mode::Active"))?
    .extract(
        "fn run_active(&mut self, input: &Input) -> usize",
        &["input"],
    )?;
```

The supplied signature defines the helper, and `arguments` supplies its call expressions. A `self` receiver is carried by the generated method call; the argument list corresponds to the remaining parameters.

The helper is inserted beside the enclosing function. Associated helpers use the enclosing implementation or trait context. An async signature produces an awaited call.

Extraction creates a function scope. The signature, arguments and output bindings express the patch's choices about moves, borrows and local destruction.

### Carrying control flow

Options precede `extract`:

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

`control_flow()` carries `return`, `break` and `continue` whose destinations are outside the selection. The call site performs the original action, including its label and value. Exits belonging to loops or closures inside the selection travel with their owner.

One distinct external exit uses its payload directly in `core::ops::ControlFlow`. Several exit kinds or destinations use a generated enum named after the helper, such as `ParseItemExit`, with the helper's visibility.

`propagate()` carries `?` through the enclosing `Result`, `Option` or `ControlFlow` container and applies `?` at the generated call. Available aliases and interface declarations participate in resolving that container.

The signature describes normal completion. For a normal result `Output`, combined options in a `Result` context produce a shape such as `Result<ControlFlow<Exit, Output>, Error>`. Exit payload types come from the enclosing return declaration, a loop destination's type or the value's declaration.

### Returning local bindings

`outputs` names bindings created within the selected code and describes how to receive them:

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

Extract the declarations and increment before `report`:

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

For a statement region, the call site receives these values as:

```rust
let (name, mut count) = parse_name(raw);
```

Each entry is an identifier with an optional `mut`. The name is associated with its visible declaration at the end of the region; that declaration must belong to the selected code. The receiver's mutability is explicit because mutation may occur on different sides of the extraction.

A single output uses its own return type. Multiple outputs use a tuple in the listed order. When the selected code also yields an ordinary expression result, that result occupies the first tuple element, followed by the outputs. The original expression receives that leading value.

The supplied signature describes this complete normal result. `control_flow()` and `propagate()` wrap it as above. Newly declared outputs are returned on normal completion, while earlier exits follow their corresponding control-flow paths.

Inputs already declared outside the selection use the supplied parameter and argument interface. Selecting such an input as a new output produces an error.

## Runnable example

[examples/edit.rs](../examples/edit.rs) extends a command dispatcher with a repeat operation. It extracts the existing print-arm implementation, adds an enum variant and routes the added match arm through that helper.

```sh
cargo run --example edit
```

The example writes the resulting Rust program to stdout. Its `main` accepts a message and repeat count.
