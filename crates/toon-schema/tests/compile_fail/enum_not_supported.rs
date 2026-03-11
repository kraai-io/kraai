use toon_schema::toon_tool;

toon_tool! {
    name: "bad_enum",
    types: {
        enum Bad {
            One,
        }
    },
    root: Bad,
    examples: []
}

fn main() {}
