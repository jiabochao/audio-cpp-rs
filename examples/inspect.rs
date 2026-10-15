//! Lists the families compiled into libaudiocpp, or describes one model.
//!
//! cargo run --example inspect                       # families and tasks
//! cargo run --example inspect -- <model> [family]   # a model's capabilities and options

use audio_cpp::{Mode, ModelConfig, OptionScope, Registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("audio.cpp {} (ABI {})", audio_cpp::build_version(), audio_cpp::abi_version());
    let registry = Registry::new()?;

    let mut args = std::env::args().skip(1);
    let Some(model_path) = args.next() else {
        println!("tasks: {}", audio_cpp::tasks().join(", "));
        println!("families:");
        for family in registry.families() {
            println!("  {family}");
        }
        return Ok(());
    };

    let config = ModelConfig { family: args.next(), ..Default::default() };
    let model = registry.load_model(&model_path, &config, None)?;
    println!("family: {}", model.family());
    println!("description: {}", model.description());
    for mode in [Mode::Offline, Mode::Streaming] {
        let tasks: Vec<_> = audio_cpp::tasks().into_iter().filter(|t| model.supports(t, mode)).collect();
        println!("{} tasks: {}", mode.as_str(), tasks.join(", "));
    }
    println!("languages: {}", model.languages().join(", "));
    for (scope, name) in
        [(OptionScope::Load, "load"), (OptionScope::Session, "session"), (OptionScope::Request, "request")]
    {
        let options = model.options(scope)?;
        if options.is_empty() {
            continue;
        }
        println!("{name} options:");
        for option in options {
            let required = if option.required { " (required)" } else { "" };
            println!("  --{} <{}> default={:?}{required}", option.name, option.value_name, option.default_value);
            if !option.description.is_empty() {
                println!("      {}", option.description);
            }
        }
    }
    Ok(())
}
