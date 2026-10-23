//! Manual/integration driver restricted to the caller's owned test compositor.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    use uu_input::{InputEvent as E, InputInjector, KeyCode, MouseButton};
    anyhow::ensure!(
        std::env::var("UU_OWNED_WAYLAND_TEST").as_deref() == Ok("1"),
        "requires an explicitly owned compositor"
    );
    let runtime = std::env::var("XDG_RUNTIME_DIR")?;
    anyhow::ensure!(
        std::path::Path::new(&runtime)
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("uu-wayland-input-")),
        "requires an isolated test runtime"
    );
    let mut args = std::env::args().skip(1);
    let output = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("output name required"))?;
    let width = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("width required"))?
        .parse()?;
    let height = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("height required"))?
        .parse()?;
    let mut injector = uu_input::wayland::WlrootsInjector::connect(
        (width, height),
        uu_input::wayland::OutputTarget {
            name: Some(output),
            ..Default::default()
        },
    )?;
    println!("{{\"ready\":true}}");
    std::io::stdout().flush()?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let a: Vec<_> = line.split_whitespace().collect();
        let event = match a.as_slice() {
            ["move", x, y] => Some(E::MouseMoveAbsolute {
                x: x.parse()?,
                y: y.parse()?,
            }),
            ["relative", x, y] => Some(E::MouseMoveRelative {
                dx: x.parse()?,
                dy: y.parse()?,
            }),
            ["key", key, pressed] => Some(E::Key {
                key: KeyCode(key.parse()?),
                pressed: pressed.parse::<u8>()? != 0,
            }),
            ["button", button, pressed] => Some(E::MouseButton {
                button: match *button {
                    "left" => MouseButton::Left,
                    "right" => MouseButton::Right,
                    _ => anyhow::bail!("unknown button"),
                },
                pressed: pressed.parse::<u8>()? != 0,
            }),
            ["scroll", x, y] => Some(E::MouseScroll {
                dx: x.parse()?,
                dy: y.parse()?,
            }),
            ["release"] => {
                injector.release_all()?;
                None
            }
            ["quit"] => break,
            _ => anyhow::bail!("unknown owned probe command"),
        };
        if let Some(e) = event {
            injector.inject(e)?;
        }
        println!("{{\"ok\":true}}");
        std::io::stdout().flush()?;
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {}
