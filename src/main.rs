mod email_tools;

fn main() {
    let e = email_tools::get_inbox_one(17373);

    match e {
        Ok(email) => {
            println!("{:#?}", email);
        }
        Err(error) => {
            println!("Could not retrieve message!");
            eprintln!("{}", error);
        }
    }
}
