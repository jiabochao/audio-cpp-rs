//! Lists the families compiled into libaudiocpp, or describes one model.
//!
//! cargo run --example inspect                       # families and tasks
//! cargo run --example inspect -- <model> [family]   # a model's capabilities and options

use audio_cpp::{ModelConfig, OptionInfo, Registry, Task};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("audio.cpp {} (ABI {})", audio_cpp::build_version(), audio_cpp::abi_version());
    let registry = Registry::new()?;

    let mut args = std::env::args().skip(1);
    let Some(model_path) = args.next() else {
        let tasks: Vec<String> = Task::all().iter().map(Task::to_string).collect();
        println!("tasks: {}", tasks.join(", "));
        println!("families: {}", registry.families()?.join(", "));
        return Ok(());
    };

    let config = ModelConfig { family: args.next(), ..Default::default() };
    let info = registry.load_model(&model_path, &config, None)?.info()?;
    let names = |tasks: &[Task]| tasks.iter().map(Task::to_string).collect::<Vec<_>>().join(", ");
    println!("family:      {}", info.family);
    println!("description: {}", info.description);
    println!("offline:     {}", names(&info.offline_tasks));
    println!("streaming:   {}", names(&info.streaming_tasks));
    println!("languages:   {}", info.languages.join(", "));
    let print = |scope: &str, options: &[OptionInfo]| {
        if options.is_empty() {
            return;
        }
        println!("{scope} options:");
        for option in options {
            let required = if option.required { " (required)" } else { "" };
            println!("  {} <{}> default={:?}{required}", option.name, option.value_name, option.default_value);
            if !option.description.is_empty() {
                println!("      {}", option.description);
            }
        }
    };
    print("load", &info.load_options);
    print("session", &info.session_options);
    print("request", &info.request_options);
    Ok(())
}
