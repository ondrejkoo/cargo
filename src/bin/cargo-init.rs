#![crate_id="cargo-init"]
#![feature(phase)]

use std::io::{stdin, UserRWX, IoResult};
use std::io::fs::{File, readdir, mkdir};
use std::os;

/// Checks the existance of a file with a given extension.
fn already_exists(items: Vec<Path>, quer: &str) -> bool {
    items.iter().any(|nms: &Path| -> bool {
        let ext_str = nms.extension_str();
        match ext_str{
            Some(a) => a == quer, //file with extension found
            None => false // file has no extension
        }
    })
}

/// Prompts the user for command-line input, with a default response.
fn ask_default(query: String, default: String) -> IoResult<String> {
    print!("{} [default: {}] ", query, default);
    let result = try!(stdin().read_line());
    if result == "\n".to_str() {
        Ok(default)
    }
    else {
        Ok(result.as_slice().slice_from(1).to_str())
    }
}

/// Initalize a Cargo build.
fn execute() -> IoResult<()>{
    let os_args  = os::args();
    let mut args = os_args.slice(1, os_args.len()).iter().map(|x| x.as_slice()); // Drop exec name

    let cwd_contents = try!(readdir(&os::getcwd())); 

    if already_exists(cwd_contents, "toml") && ! args.any(|x| x == "--override"){
        println!(".toml file already exists in current directory. Use --override to bypass.");
        return Ok(());
    }
    
    //Either explicitly state a name, type, and author, or all will be gathered interactively.
    try!(match os_args.as_slice().slice(1,os_args.len()) {
        [ref nm, ref bl, ref auth, ..] if *nm != "--override".to_str() => 
             make_cargo(nm.as_slice(), bl == &"lib".to_str(), auth.as_slice()),
        _ => make_cargo_interactive()
    })
    Ok(())
}

/// Prompts the user to name and license their project.
fn make_cargo_interactive() -> IoResult<()>{
    try!(make_cargo(
        try!(ask_default("Project name:".to_str(), "untitled".to_str())).as_slice(),
        try!(ask_default("lib/bin:".to_str(), "lib".to_str())).as_slice() == "lib".as_slice(),
        try!(ask_default("Your name: ".to_str(), "anonymous".to_str())).as_slice()));
    Ok(())
}

/// Write the .toml file and set up the .src directory with a dummy file
fn make_cargo(nm: &str, lib: bool, auth: &str) -> IoResult<()>{
    let cwd_contents = readdir(&os::getcwd()).unwrap();
    let mut tomlFile = File::create(&Path::new("Cargo.toml"));
    let mut gitignr  = File::create(&Path::new(".gitignore")); // Ignores "target" by default"

    try!(mkdir(&Path::new("src"), UserRWX));
    let mut srcFile = File::create(&Path::new(format!("./src/{}.rs", nm)));
    
    if !cwd_contents.iter().any(|x| x.filename_str() == Some(".gitignore")) {
        try!(srcFile.write("".as_bytes()));
        try!(gitignr.write("target".as_bytes()));
    }

    try!(tomlFile.write       ("[package]\n".as_bytes()));
    try!(tomlFile.write(format!("name = \"{}\"\n", nm).as_bytes()));
    try!(tomlFile.write        ("version = \"0.1.0\"\n".as_bytes()));

    try!(tomlFile.write(format!("authors = [ \"{}\" ]\n\n", auth).as_bytes()));
    
    if lib {
        try!(tomlFile.write("[[lib]]\n".as_slice().as_bytes())); //No main in src
    } else
    {
        try!(tomlFile.write("[[bin]]\n".as_slice().as_bytes()));
        try!(srcFile.write("fn main() {\n\n}".as_bytes()));
    }

    try!(tomlFile.write(format!("name = \"{}\"\n", nm).as_slice().as_bytes()));    
    Ok(())
}

fn main() { 
    let val = execute();
    match val{
        Ok(()) => return,
        _ => fail!("Execute reports failure")
    }
}
