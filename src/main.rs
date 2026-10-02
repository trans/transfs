use std::{
    env,
    error::Error,
    io::{self, Write},
    path::{Path, PathBuf},
};
use transfs::{
    check::check,
    claim::format_ts,
    document::Document,
    index::{Index, Row},
    library::Library,
    mount,
};

type CliResult<T> = Result<T, Box<dyn Error>>;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run() -> CliResult<i32> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let root = if args.first().is_some_and(|a| a == "--store") {
        args.remove(0);
        PathBuf::from(required(&mut args, "--store needs a directory")?)
    } else if let Ok(root) = env::var("TRANSFS_STORE") {
        PathBuf::from(root)
    } else {
        env::current_dir()?.join("test/store")
    };
    let root = if root.is_absolute() {
        root
    } else {
        env::current_dir()?.join(root)
    };
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("transfs [--store DIR] <command> [args]\n\ncommands: add addversion rename tag untag set list find reindex check cat show versions mount");
        return Ok(0);
    }
    let command = args.remove(0);
    let lib = Library::new(&root);
    match command.as_str() {
        "add" => {
            let file = required(&mut args, "add needs a file")?;
            let name = args.first().map(String::as_str);
            let doc = lib.add(Path::new(&file), name)?;
            update_index(&root, &doc)?;
            println!(
                "added {}  \"{}\"  ({} version, head {})",
                short(&doc.id),
                doc.name.as_deref().unwrap_or(""),
                doc.version_count(),
                doc.head().map(short).unwrap_or("")
            );
        }
        "addversion" => {
            let doc = resolve(&lib, &required(&mut args, "addversion needs an id")?)?;
            let file = required(&mut args, "addversion needs a file")?;
            let doc = lib.add_version(&doc, Path::new(&file))?;
            update_index(&root, &doc)?;
            println!(
                "added version {} to {} (now v{})",
                doc.head().map(short).unwrap_or(""),
                short(&doc.id),
                doc.version_count()
            );
        }
        "rename" => {
            let doc = resolve(&lib, &required(&mut args, "rename needs an id")?)?;
            let name = required(&mut args, "rename needs a name")?;
            let doc = lib.rename(&doc, &name)?;
            update_index(&root, &doc)?;
            println!("renamed {} -> \"{}\"", short(&doc.id), name);
        }
        "tag" | "untag" => {
            let doc = resolve(&lib, &required(&mut args, "tag needs an id")?)?;
            if args.first().is_some_and(|a| a == "--") {
                args.remove(0);
            }
            if args.is_empty() {
                return Err("no tags given".into());
            }
            let doc = if command == "tag" {
                lib.tag(&doc, &args, &[])?
            } else {
                lib.tag(&doc, &[], &args)?
            };
            update_index(&root, &doc)?;
            println!(
                "tags: {}",
                doc.tags.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        "set" => {
            let doc = resolve(&lib, &required(&mut args, "set needs an id")?)?;
            let key = required(&mut args, "set needs a key")?;
            let value = required(&mut args, "set needs a value")?;
            let doc = lib.set_tag(&doc, &key, &value)?;
            update_index(&root, &doc)?;
            println!(
                "tags: {}",
                doc.tags.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        "list" => print_rows(Index::open(&root)?.all()?),
        "find" => {
            let query = required(&mut args, "find needs a query")?;
            let index = Index::open(&root)?;
            let rows = if let Some(key) = query.strip_prefix("tag:") {
                index.by_tag(key)?
            } else if let Some(prefix) = query.strip_prefix("type:") {
                index.by_type(prefix)?
            } else if let Some(name) = query.strip_prefix("name:") {
                index.by_name(name)?
            } else {
                index.by_name(&query)?
            };
            print_rows(rows);
        }
        "reindex" => {
            let mut index = Index::open(&root)?;
            index.rebuild()?;
            for warning in &index.rebuild_warnings {
                println!(
                    "warning: {}: line {}: ignored torn trailing record",
                    warning.path.display(),
                    warning.line
                );
            }
            for error in &index.rebuild_errors {
                println!("error: {error}");
            }
            println!("reindexed {} documents", index.all()?.len());
            if !index.rebuild_errors.is_empty() {
                return Ok(1);
            }
        }
        "check" => {
            let result = check(&root)?;
            for warning in &result.warnings {
                println!("warning: {}: {}", warning.path.display(), warning.message);
            }
            for error in &result.errors {
                println!("error: {}: {}", error.path.display(), error.message);
            }
            if result.clean() {
                println!("ok: {} documents, {} blobs", result.documents, result.blobs);
            } else {
                return Ok(1);
            }
        }
        "cat" => {
            let doc = resolve(&lib, &required(&mut args, "cat needs an id")?)?;
            let bytes = lib.read(&doc)?.ok_or("document has no content")?;
            io::stdout().write_all(&bytes)?;
        }
        "show" => {
            let doc = resolve(&lib, &required(&mut args, "show needs an id")?)?;
            println!("id:        {}", doc.id);
            println!("name:      {}", doc.name.as_deref().unwrap_or(""));
            println!(
                "created:   {}",
                doc.created_at.map(format_ts).unwrap_or_default()
            );
            println!("versions:  {}", doc.version_count());
            println!("head:      {}", doc.head().unwrap_or(""));
            println!(
                "tags:      {}",
                doc.tags.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        "versions" => {
            let doc = resolve(&lib, &required(&mut args, "versions needs an id")?)?;
            if doc.versions.is_empty() {
                println!("(no versions)");
            }
            for (i, version) in doc.versions.iter().enumerate() {
                let marker = if i + 1 == doc.versions.len() {
                    "* "
                } else {
                    "  "
                };
                let parent = version.parent.as_deref().map(short).unwrap_or("(root)");
                println!(
                    "{marker}v{}  {}  parent={}  {}",
                    i + 1,
                    short(&version.hash),
                    parent,
                    format_ts(version.ts)
                );
            }
        }
        "mount" => {
            let mountpoint = required(&mut args, "mount needs a mountpoint")?;
            let mountpoint = std::fs::canonicalize(&mountpoint)?;
            if !mountpoint.is_dir() {
                return Err("mountpoint is not a directory".into());
            }
            mount::mount(&root, &mountpoint)?;
        }
        _ => return Err(format!("unknown command: {command}").into()),
    }
    Ok(0)
}

fn required(args: &mut Vec<String>, message: &str) -> CliResult<String> {
    if args.is_empty() {
        Err(message.to_owned().into())
    } else {
        Ok(args.remove(0))
    }
}
fn resolve(lib: &Library, id: &str) -> CliResult<Document> {
    Ok(lib
        .document(id)?
        .ok_or_else(|| format!("no such document: {id}"))?)
}
fn update_index(root: &Path, doc: &Document) -> CliResult<()> {
    Index::open(root)?.index_document(doc)?;
    Ok(())
}
fn short(s: &str) -> &str {
    s.get(..12).unwrap_or(s)
}
fn print_rows(rows: Vec<Row>) {
    if rows.is_empty() {
        println!("(none)");
        return;
    }
    for row in rows {
        println!(
            "{}  {:<24}  {:<16}  v{}  {}",
            short(&row.id),
            row.name.as_deref().unwrap_or("(unnamed)"),
            row.mime_type.as_deref().unwrap_or(""),
            row.version_count,
            row.tags.join(",")
        );
    }
}
