/// Make a path to a dependency, which aligns to
///
/// - [index from of Cargo's index on filesystem][1], and
/// - [index from Crates.io][2].
///
/// [1]: https://docs.rs/cargo/latest/cargo/sources/registry/index.html#the-format-of-the-index
/// [2]: https://github.com/rust-lang/crates.io-index
pub fn make_dep_path(dep_name: &str, prefix_only: bool) -> String {
    let (slash, name) = if prefix_only {
        ("", "")
    } else {
        ("/", dep_name)
    };
    match dep_name.chars().take(4).count() {
        1 => format!("1{}{}", slash, name),
        2 => format!("2{}{}", slash, name),
        3 => {
            let first_symbol = dep_name.chars().take(1).collect::<String>();

            format!("3/{}{}{}", first_symbol, slash, name)
        }
        _ => {
            let first_symbol = dep_name.chars().take(2).collect::<String>();
            let second_symbol = dep_name.chars().skip(2).take(2).collect::<String>();

            format!("{}/{}{}{}", first_symbol, second_symbol, slash, name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::make_dep_path;

    #[test]
    fn prefix_only() {
        assert_eq!(make_dep_path("a", true), "1");
        assert_eq!(make_dep_path("ab", true), "2");
        assert_eq!(make_dep_path("abc", true), "3/a");
        assert_eq!(make_dep_path("Abc", true), "3/A");
        assert_eq!(make_dep_path("AbCd", true), "Ab/Cd");
        assert_eq!(make_dep_path("aBcDe", true), "aB/cD");
    }

    #[test]
    fn full() {
        assert_eq!(make_dep_path("a", false), "1/a");
        assert_eq!(make_dep_path("ab", false), "2/ab");
        assert_eq!(make_dep_path("abc", false), "3/a/abc");
        assert_eq!(make_dep_path("Abc", false), "3/A/Abc");
        assert_eq!(make_dep_path("AbCd", false), "Ab/Cd/AbCd");
        assert_eq!(make_dep_path("aBcDe", false), "aB/cD/aBcDe");
    }

    #[test]
    fn test_10993() {
        assert_eq!(make_dep_path("ĉa", true), "2");
        assert_eq!(make_dep_path("abcĉ", true), "ab/cĉ");

        assert_eq!(make_dep_path("ĉa", false), "2/ĉa");
        assert_eq!(make_dep_path("abcĉ", false), "ab/cĉ/abcĉ");
    }
}
