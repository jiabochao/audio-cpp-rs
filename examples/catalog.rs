//! Browses the embedded model catalog: what audio.cpp can run and where to
//! download it. Needs no native library call and no network.
//!
//! cargo run --example catalog                  # every family, by task
//! cargo run --example catalog -- <task>        # families serving a task, e.g. tts, asr, vad
//! cargo run --example catalog -- <family>      # packages and download URLs for a family

use audio_cpp::Task;
use audio_cpp::catalog::{Catalog, Mirror};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = Catalog::builtin();
    let arg = std::env::args().nth(1);

    if let Some(family) = arg.as_deref().and_then(|name| catalog.family(name)) {
        println!("{} ({}) [{}]", family.display_name, family.family, family.status);
        println!("{}", family.description);
        println!("tasks: {:?}  modes: {:?}", family.tasks(), family.modes);
        let recommended = family.recommended_package().map(|p| p.id.as_str());
        for package in &family.packages {
            let mark = if Some(package.id.as_str()) == recommended { "*" } else { " " };
            println!("{mark} {} ({} {})", package.id, package.format, package.precision);
            let Some(download) = family.download(package).filter(|d| d.is_downloadable()) else {
                println!("    not downloadable");
                continue;
            };
            for file in package.files()? {
                println!("    {}", file.local.display());
                for mirror in [Mirror::hugging_face(), Mirror::model_scope()] {
                    if let Some(url) = mirror.url(download, &file) {
                        println!("      {url}");
                    }
                }
            }
        }
        return Ok(());
    }

    let tasks: Vec<Task> = match &arg {
        Some(task) => vec![Task::from(task.as_str())],
        None => vec![
            Task::Tts,
            Task::VoiceCloning,
            Task::VoiceDesign,
            Task::Asr,
            Task::Vad,
            Task::Separation,
            Task::Generation,
        ],
    };
    println!("catalog from audio.cpp {}", &catalog.source_commit[..8]);
    for task in tasks {
        let families: Vec<_> = catalog.serving(&task).map(|f| f.family.as_str()).collect();
        println!("{task} ({}): {}", families.len(), families.join(", "));
    }
    Ok(())
}
