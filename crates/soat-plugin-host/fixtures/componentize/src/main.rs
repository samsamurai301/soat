fn main() {
    let mut args = std::env::args().skip(1);
    let (input, output) = (args.next().unwrap(), args.next().unwrap());
    let core = std::fs::read(input).unwrap();
    let component = wit_component::ComponentEncoder::default()
        .module(&core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    std::fs::write(output, component).unwrap();
}
