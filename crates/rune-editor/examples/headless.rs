//! Interactive native session, intentionally not a Lisp/terminal frontend.
use rune_editor::*;
use std::io::{self, BufRead};
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new(StringDocument::empty(), 80, 24)?;
    eprintln!("insert TEXT | find TEXT | backfind TEXT | goto N | undo | redo | split | next | show | quit");
    for line in io::stdin().lock().lines() {
        let line = line?;
        let (verb, argument) = line.split_once(' ').unwrap_or((&line, ""));
        if verb == "quit" { break; }
        if verb == "show" {
            let snapshot = session.snapshot();
            for view in snapshot.views {
                let buffer = snapshot.buffers.iter().find(|buffer| buffer.id == view.view.buffer).expect("live buffer");
                println!("window={} buffer={} point={} {:?}", view.id.as_u64(), buffer.name, view.view.point, buffer.document.to_text());
            }
            continue;
        }
        let command = match verb {
            "insert" => Command::Insert(argument.into()),
            "find" => Command::FindForward(argument.into()),
            "backfind" => Command::FindBackward(argument.into()),
            "goto" => match argument.parse() { Ok(point) => Command::GoTo(point), Err(_) => { eprintln!("invalid scalar position"); continue; } },
            "undo" => Command::Undo, "redo" => Command::Redo,
            "split" => Command::Split(Axis::Vertical), "next" => Command::NextWindow,
            _ => { eprintln!("unknown command"); continue; }
        };
        if let Err(error) = session.execute(command) { eprintln!("command failed: {error}"); }
        session.check_invariants()?;
    }
    Ok(())
}
