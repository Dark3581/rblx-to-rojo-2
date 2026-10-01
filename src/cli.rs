use log::{info, warn, Log};
use rbxlx_to_rojo::{display_path, filesystem::FileSystem, process_instructions_with, Options};
use std::{
    fmt, fs,
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const LOG_FILE: &str = "rbxlx-to-rojo.log";

#[derive(Debug)]
enum Problem {
    BinaryDecodeError(rbx_binary::DecodeError),
    DialogCancelled,
    InvalidFile,
    IoError(&'static str, io::Error),
    WriteErrors(usize),
    XMLDecodeError(rbx_xml::DecodeError),
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Problem::BinaryDecodeError(error) => write!(
                formatter,
                "While attempting to decode the place file, at {} rbx_binary didn't know what to do",
                error,
            ),

            Problem::InvalidFile => {
                write!(formatter, "The file provided does not have a recognized file extension")
            }

            Problem::IoError(doing_what, error) => {
                write!(formatter, "While attempting to {}, {}", doing_what, error)
            }

            Problem::DialogCancelled => write!(
                formatter,
                "Cancelled without selecting a place file or output folder.",
            ),

            Problem::WriteErrors(count) => write!(
                formatter,
                "{} file(s) couldn't be written; see {} for which ones.",
                count, LOG_FILE,
            ),

            Problem::XMLDecodeError(error) => write!(
                formatter,
                "While attempting to decode the place file, at {} rbx_xml didn't know what to do",
                error,
            ),
        }
    }
}

/// Lines logged before we know where the log file goes are kept in memory,
/// then written out once it's created, so the file has the whole run.
enum LogSink {
    Buffer(Vec<String>),
    File(fs::File),
}

struct WrappedLogger {
    log: env_logger::Logger,
    sink: Arc<Mutex<LogSink>>,
}

/// The only properties the export reads. A decoding problem anywhere else
/// can't change the output.
const EXPORTED_PROPERTIES: &[&str] = &["Source", "Attributes", "Tags", "RunContext", "Disabled", "Enabled"];

/// rbx_binary warns about every property whose type is newer than it knows,
/// e.g. `Unknown value type ID 0x23 (35) in Roblox binary model file. Found in
/// property Terrain.VoxelGridAssetContentMap.` Returns the `Class.Property`
/// part if `message` is one of those.
fn unknown_property_type(message: &str) -> Option<&str> {
    if !message.starts_with("Unknown value type ID") {
        return None;
    }

    let property = message.split("Found in property ").nth(1)?;
    Some(property.trim_end_matches('.'))
}

impl WrappedLogger {
    fn write(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        self.log.log(record);

        let line = format!("[{:<5} {}] {}", record.level(), record.target(), record.args());
        match &mut *self.sink.lock().unwrap() {
            LogSink::Buffer(lines) => lines.push(line),
            LogSink::File(file) => {
                file.write_all(format!("{}\r\n", line).as_bytes()).ok();
            }
        }
    }
}

impl log::Log for WrappedLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.log.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        let message = record.args().to_string();

        // Roblox adds property types faster than the decoder learns them. When
        // the property isn't one the export reads, say so plainly instead of
        // passing on a warning that looks like a failure.
        if record.target().starts_with("rbx_binary") {
            if let Some(property) = unknown_property_type(&message) {
                let name = property.rsplit('.').next().unwrap_or(property);
                if !EXPORTED_PROPERTIES.contains(&name) {
                    self.write(
                        &log::Record::builder()
                            .level(log::Level::Info)
                            .target(env!("CARGO_CRATE_NAME"))
                            .args(format_args!(
                                "Skipped {} (a newer property type the file reader doesn't support yet). \
                                 The export doesn't use it, so nothing is lost.",
                                property
                            ))
                            .build(),
                    );
                    return;
                }
            }
        }

        self.write(record);
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::unknown_property_type;

    #[test]
    fn recognizes_unknown_property_type_warning() {
        assert_eq!(
            unknown_property_type(
                "Unknown value type ID 0x23 (35) in Roblox binary model file. Found in property Terrain.VoxelGridAssetContentMap."
            ),
            Some("Terrain.VoxelGridAssetContentMap")
        );
        assert_eq!(unknown_property_type("Something else entirely"), None);
    }
}

fn open_log_file(sink: &Mutex<LogSink>, path: &Path) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    let mut sink = sink.lock().unwrap();

    if let LogSink::Buffer(lines) = &*sink {
        for line in lines {
            file.write_all(format!("{}\r\n", line).as_bytes())?;
        }
    }

    *sink = LogSink::File(file);
    Ok(())
}

struct Args {
    place: Option<PathBuf>,
    output: Option<PathBuf>,
    options: Options,
}

fn parse_args() -> Args {
    let mut positional = Vec::new();
    let mut options = Options::default();

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--luau" => options.luau = true,
            "-h" | "--help" => {
                println!("Usage: rbxlx-to-rojo [--luau] [PLACE_FILE] [OUTPUT_FOLDER]");
                println!();
                println!("Converts a .rbxl/.rbxlx/.rbxm/.rbxmx file into a Rojo project.");
                println!("Leave out the file or folder to pick them in a dialog.");
                println!();
                println!("  --luau   write .luau files instead of .lua");
                println!();
                println!("The project goes in OUTPUT_FOLDER/<place file name>, and the log in");
                println!("OUTPUT_FOLDER/{}. Set RUST_LOG=debug for more detail.", LOG_FILE);
                std::process::exit(0);
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }

    let mut positional = positional.into_iter();
    Args {
        place: positional.next(),
        output: positional.next(),
        options,
    }
}

fn routine(args: Args, sink: &Mutex<LogSink>) -> Result<(), Problem> {
    info!("rbxlx-to-rojo {}", env!("CARGO_PKG_VERSION"));

    let file_path = match args.place {
        Some(path) => path,
        None => {
            info!("Select a place file.");
            rfd::FileDialog::new()
                .add_filter("Roblox place/model", &["rbxl", "rbxm", "rbxlx", "rbxmx"])
                .pick_file()
                .ok_or(Problem::DialogCancelled)?
        }
    };

    info!("Opening {}", file_path.display());
    let file_source = BufReader::new(
        fs::File::open(&file_path)
            .map_err(|error| Problem::IoError("read the place file", error))?,
    );
    info!("Decoding place file, this is the longest part...");

    let extension = file_path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase());

    let tree = match extension.as_deref() {
        Some("rbxmx") | Some("rbxlx") => {
            rbx_xml::from_reader_default(file_source).map_err(Problem::XMLDecodeError)
        }
        Some("rbxm") | Some("rbxl") => {
            rbx_binary::from_reader(file_source).map_err(Problem::BinaryDecodeError)
        }
        _ => Err(Problem::InvalidFile),
    }?;

    let root = match args.output {
        Some(path) => path,
        None => {
            info!("Select the folder to put your Rojo project in.");
            let start_dir = file_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            rfd::FileDialog::new()
                .set_directory(start_dir)
                .pick_folder()
                .ok_or(Problem::DialogCancelled)?
        }
    };

    fs::create_dir_all(&root).map_err(|error| Problem::IoError("create the output folder", error))?;
    open_log_file(sink, &root.join(LOG_FILE))
        .map_err(|error| Problem::IoError("create the log file", error))?;

    let project_root = root.join(file_path.file_stem().unwrap_or_else(|| "project".as_ref()));
    info!("Writing the Rojo project to {}", display_path(&project_root));

    let has_old_files = fs::read_dir(&project_root)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if has_old_files {
        warn!(
            "{} already has files in it. Files from an earlier run that this run doesn't overwrite \
             (e.g. scripts you've since deleted in Studio) will stay and Rojo will sync them back. \
             Delete the folder first for a clean export.",
            display_path(&project_root)
        );
    }

    let mut filesystem = FileSystem::from_root(project_root);

    info!("Starting processing, please wait a bit...");
    process_instructions_with(&tree, &mut filesystem, &args.options);

    if !filesystem.errors().is_empty() {
        return Err(Problem::WriteErrors(filesystem.errors().len()));
    }

    info!("Done! Check {} for a full log.", display_path(&root.join(LOG_FILE)));
    Ok(())
}

fn main() {
    let args = parse_args();
    // Double-clicked (no arguments): keep the window open so the result can be read.
    let interactive = args.place.is_none();

    let env_logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    let max_level = env_logger.filter();
    let sink = Arc::new(Mutex::new(LogSink::Buffer(Vec::new())));

    log::set_boxed_logger(Box::new(WrappedLogger {
        log: env_logger,
        sink: Arc::clone(&sink),
    }))
    .unwrap();
    log::set_max_level(max_level);

    let result = routine(args, &sink);

    if let Err(error) = &result {
        log::error!("An error occurred while using rbxlx-to-rojo: {}", error);
    }

    if interactive {
        print!("Press Enter to exit...");
        io::stdout().flush().ok();
        io::stdin().read_line(&mut String::new()).ok();
    }

    if result.is_err() {
        std::process::exit(1);
    }
}
