
/// `aiTerminal config [path]` — show config location + current values.
pub fn config(args: &[String]) -> i32 {
    let created = crate::config::Config::ensure_default();
    let path = crate::config::Config::path();
    match args.first().map(String::as_str) {
        None => {}
        Some("path") => {
            println!("{}", path.display());
            return 0;
        }
        // Anything else is a typo, and printing the config anyway taught people the
        // word they used was a real one. `@config paht` looked like it worked.
        Some(other) => {
            eprintln!("aiTerminal: '{other}' is not a config subcommand \u{2014} try `@config` or `@config path`");
            return 2;
        }
    }
    let c = crate::config::Config::load();
    if created {
        println!("created default config at {}", path.display());
    }
    println!("config: {}", path.display());
    println!("  theme       = {}", c.theme);
    println!("  font_family = {}", c.font_family);
    println!("  font_size   = {}", c.font_size);
    println!("  zoom        = {}", c.zoom);
    println!("  tab_bar     = {}", c.tab_bar);
    println!("  shell       = {}", if c.shell.is_empty() { "$SHELL".to_string() } else { c.shell.clone() });
    println!("  scrollback  = {}", c.scrollback);
    println!("\nedit the file, then reload in the app with Cmd-, (or restart)");
    0
}
