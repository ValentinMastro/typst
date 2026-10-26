use std::io::{self, Read};

/// Wraps a string literal in ANSI escape codes for bright cyan text.
macro_rules! cyan {
    ($s:expr) => {
        concat!("\x1b[96m", $s, "\x1b[39m")
    };
}

/// This is shown to users who just type `typst` the first time.
#[rustfmt::skip]
const GREETING: &str = concat!(
    bold!("Welcome to Typst, we are glad to have you here!"), " ❤️\n",
    "\n",
    "If you are new to Typst, ", bold!("start with the tutorial"), " at ",
    underline!("https://typst.app/docs/tutorial/"), ". To get a quick start with your first ",
    "project, ", bold!("choose a template"), " on ", underline!("https://typst.app/universe/"), ".\n",
    "\n",
    "Here are the ", bold!("most important commands"), " you will be using:\n",
    "\n",
    "- Compile a file once: ", cyan!("typst compile file.typ"), "\n",
    "- Compile a file on every change: ", cyan!("typst watch file.typ"), "\n",
    "- Set up a project from a template: ", cyan!("typst init @preview/<TEMPLATE>"), "\n",
    "\n",
    "Learn more about these commands by running ", cyan!("typst help"), ".\n",
    "\n",
    "If you have a question, we and our community would be glad to help you out on ",
    "the ", bold!("Typst Forum"), " at ", underline!("https://forum.typst.app/"), ".\n",
    "\n",
    "Happy Typsting!\n",
);

/// Greets (and exists) if not yet greeted.
pub fn greet() {
    let Some(data_dir) = dirs::data_dir() else { return };
    let path = data_dir.join("typst").join("greeted");

    let version = typst::utils::version().raw();
    let prev_greet = std::fs::read_to_string(&path).ok();
    if prev_greet.as_deref() == Some(version) {
        return;
    }

    std::fs::write(&path, version).ok();
    print_and_exit(GREETING);
}

/// Prints a colorized and line-wrapped message.
fn print_and_exit(message: &'static str) -> ! {
    // Abuse clap for line wrapping ...
    let err = clap::Command::new("typst")
        .max_term_width(80)
        .help_template("{about}")
        .about(message)
        .try_get_matches_from(["typst", "--help"])
        .unwrap_err();
    let _ = err.print();

    // Windows users might have double-clicked the .exe file and have no chance
    // to read it before the terminal closes.
    if cfg!(windows) {
        pause();
    }

    std::process::exit(err.exit_code());
}

/// Waits for the user.
#[expect(clippy::unused_io_amount)]
fn pause() {
    eprintln!();
    eprintln!("Press enter to continue...");
    io::stdin().lock().read(&mut [0]).unwrap();
}
