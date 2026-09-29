use r#override::{Edition, Result, Source, item, root};

fn main() -> Result<()> {
    let mut source = Source::parse(
        r#"
macro_rules! wrap { ($($item:item)*) => { $($item)* }; }

wrap! {
    struct State {
        value: u32,
    }

    impl State {
        fn total(&self) -> u32 {
            let State { value } = *self;
            let seed = value;
            let increment = 2;
            seed + increment
        }

        fn route(&self, mode: Mode) -> u32 {
            match mode {
                Mode::Local => self.compute(1),
                Mode::Remote => self.compute(2),
                _ => 0,
            }
        }

        fn compute(&self, value: u32) -> u32 { self.value + value }

        fn parse(&self, values: &[&str], output: &mut Vec<i32>) -> ParseResult<usize> {
            'items: for text in values {
                let n = text.parse::<i32>()?;
                if n < 0 { continue 'items; }
                if n == 0 { break 'items; }
                if n == 99 { return Ok(output.len()); }
                output.push(n);
            }
            output.push(100);
            Ok(output.len())
        }
    }
}

enum Mode { Local, Remote, Unknown }
type ParseResult<T> = Result<T, std::num::ParseIntError>;

struct Size(u32, u32);
impl Size {
    fn height(&self) -> u32 { self.1 }
}

struct Pair(u8, u8);

mod callbacks {
    pub fn dispatch(on_error: impl FnOnce(u32) -> u32, on_ready: impl FnOnce(u32) -> u32) -> u32 {
        let _ = on_error;
        on_ready(1)
    }

    pub fn forward(on_error: impl FnOnce(u32) -> u32, on_ready: impl FnOnce(u32) -> u32) -> u32 {
        dispatch(on_error, on_ready) + 1
    }

    pub fn identity(value: u32) -> u32 { value }
}

use callbacks::{dispatch as send, identity};

fn run() -> u32 {
    let callback = send;
    callback(|x| x + 5, |x| x + 5)
}

fn once(callback: impl FnOnce() -> u32) -> u32 { callback() }

fn decorate(bias: u32, callback: impl FnOnce(u32) -> u32) -> impl FnOnce(u32) -> u32 {
    move |value| callback(value) + bias
}

fn compute(bias: u32, state: &State, value: u32) -> u32 {
    state.compute(value) + bias
}

fn main() {
    let state = State { value: 4 };
    assert_eq!(state.bias, 10);
    assert_eq!(state.total(), 7);
    assert_eq!(state.sum(20, 3), 23);
    assert_eq!(state.route(Mode::Local), 15);
    assert_eq!(state.route(Mode::Remote), 6);
    assert_eq!(state.route(Mode::External), 7);
    assert_eq!(state.route(Mode::Unknown), 0);
    for (input, expected) in [
        (&["1", "-1", "2", "0", "3"][..], vec![1, 2, 100]),
        (&["1", "99", "3"], vec![1]),
        (&["2", "3"], vec![2, 3, 100]),
    ] {
        let mut output = Vec::new();
        assert_eq!(state.parse(input, &mut output).unwrap(), expected.len());
        assert_eq!(output, expected);
    }
    let mut output = Vec::new();
    assert!(state.parse(&["1", "invalid"], &mut output).is_err());
    assert_eq!(output, [1]);
    assert_eq!(run(), 10);
    assert_eq!(identity(7), 7);
    let size = Size(7, 9);
    assert_eq!(size.0, 7);
    assert_eq!(size.height(), 9_u64);
    let pair = Pair(2, 300);
    assert_eq!((pair.0, pair.1), (2, 300_u16));
}
"#,
        Edition::Edition2024,
    )?;

    source
        .select(item("State"))?
        .at(root().field("value").after())
        .add_field("bias: u32")?;
    source
        .select(item("State::total").pattern("State"))?
        .add_rest()?;
    source
        .select(item("Mode"))?
        .at(root().variant("Remote").before())
        .add_variant("External")?;
    source
        .select(
            item("State::route")
                .match_expr()
                .has(root().arm("Mode::Local")),
        )?
        .at(root().arm("_").before())
        .add_arm("Mode::External => self.compute(3)")?;
    source
        .select(item("State").field("value"))?
        .set_visibility("pub(crate)")?;
    source
        .select(item("main").record("State"))?
        .add_field("bias: 10")?;
    source
        .select(item("State::total").region(root().binding("increment").after(), root().end()))?
        .extract(
            "fn sum(&self, seed: u32, increment: u32) -> u32",
            &["seed", "increment"],
        )?;
    source
        .select(
            item("State::total")
                .region(root().binding("increment").after(), root().end())
                .call("sum")
                .argument("seed"),
        )?
        .set_value("seed + 1")?;
    source
        .select(item("State::total").region(root().binding("increment").after(), root().end()))?
        .delegate("once", &[])?;
    source
        .select(item("State::route").arm("Mode::Local").call("compute"))?
        .delegate("compute", &["self.bias"])?;
    source
        .select(item("State::route").arm("Mode::Remote"))?
        .extract("fn remote(&self) -> u32", &[])?;
    source
        .select(item("State::parse").for_loop().body())?
        .control_flow()
        .propagate()
        .extract(
            "fn parse_item(&self, text: &str, output: &mut Vec<i32>)",
            &["text", "output"],
        )?;
    source
        .select(item("run").call("callback").argument("on_ready").closure())?
        .delegate("decorate", &["3"])?;

    source
        .select(root().import("send"))?
        .redirect("callbacks::forward")?;
    source
        .select(item("Size").field("height"))?
        .set_type("u64")?;
    source
        .select(item("Size::height"))?
        .set_return_type("u64")?;
    source.describe("Pair(left, right)")?;
    source
        .select(item("Pair").field("right"))?
        .set_type("u16")?;

    print!("{source}");
    Ok(())
}
