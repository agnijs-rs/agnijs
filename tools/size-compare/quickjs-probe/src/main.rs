//! QuickJS-NG in the size probe: one runtime with a full context, the
//! command-line script evaluated, its completion value printed.

use rquickjs::{CatchResultExt, Coerced, Context, Runtime};

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    let rt = Runtime::new().expect("runtime");
    let ctx = Context::full(&rt).expect("context");
    ctx.with(
        |ctx| match ctx.eval::<Coerced<String>, _>(src).catch(&ctx) {
            Ok(v) => println!("{}", v.0),
            Err(e) => println!("error: {e}"),
        },
    );
}
