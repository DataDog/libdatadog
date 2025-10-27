$ DS_SHORT_N=1_000 DS_MED_N=1_000 DS_LONG_N=1_000 cargo bench
running 10 tests
test benches::equivalence_sanity ... ignored
test benches::hand_long        ... bench:   1,218,778.12 ns/iter (+/- 53,255.62)
test benches::hand_med         ... bench:   1,077,039.57 ns/iter (+/- 25,587.99)
test benches::hand_short       ... bench:   1,067,602.10 ns/iter (+/- 50,716.09)
test benches::regex_lite_long  ... bench: 256,439,850.00 ns/iter (+/- 4,530,495.43)
test benches::regex_lite_med   ... bench: 115,584,504.20 ns/iter (+/- 1,970,834.13)
test benches::regex_lite_short ... bench:  57,586,325.00 ns/iter (+/- 1,287,211.63)
test benches::regex_long       ... bench:  20,388,445.90 ns/iter (+/- 780,968.75)
test benches::regex_med        ... bench:   9,923,704.15 ns/iter (+/- 376,981.10)
test benches::regex_short      ... bench:   5,797,295.85 ns/iter (+/- 299,717.17)

