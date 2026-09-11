// Manual probe: print every SMC key starting with the given prefix (default
// "T") as `key<TAB>type<TAB>value`, plus the derived CPU/GPU temperatures.
// Run it on a new chip generation to check the prefix map in smc.rs.
fn main() {
    let prefix = std::env::args().nth(1).unwrap_or_else(|| "T".into());
    for (k, ty, v) in mole_ops::smc::dump_keys(&prefix) {
        println!("{k}\t{ty}\t{v:?}");
    }
    println!("{:?}", mole_ops::smc::temperatures());
}
