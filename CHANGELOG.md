# [0.1.1](https://github.com/adamsrnmsu/ferrofire/compare/v0.1.0...v0.1.1) - 2026-11-11

### 🐛 Bug Fixes

- _(release)_ inline the zig pins so a tag-dispatched rebuild works on old tags [_(6d83ccb)_](https://github.com/adamsrnmsu/ferrofire/commit/6d83ccbcac40c9c3cd861b01feb9eb82e98cdf80)

- _(release)_ build musl binaries with zig instead of apt; apt mirror fallback in the CI image [_(6f6a53e)_](https://github.com/adamsrnmsu/ferrofire/commit/6f6a53e2a2fe134574cfa036d5504f923a17fe2c)

### 🚜 Refactor

- _(scpi)_ read analog tags with MEASure:SCALar instead of ANALog [_(77e3da6)_](https://github.com/adamsrnmsu/ferrofire/commit/77e3da60b2c95cffc654ee95998f1614defc505b)

# [0.1.0](https://github.com/adamsrnmsu/ferrofire/releases/tag/v0.1.0) - 2026-11-11

### 🚀 Features

- _(cli)_ ferrofire scpi, a client for SCPI instruments [_(7c283b3)_](https://github.com/adamsrnmsu/ferrofire/commit/7c283b3d5d0cd238341b079af9c146b5c38bf54c)

- _(fd)_ SCPI instrument server (<scpi-server>, proto-scpi) [_(772a526)_](https://github.com/adamsrnmsu/ferrofire/commit/772a5261cf6f85746c1dd9da9e0d581c9845118d)

- _(scpi)_ program message syntax (units, headers, parameters) shared by server and client [_(4169f5b)_](https://github.com/adamsrnmsu/ferrofire/commit/4169f5b202321f52db7c1fa7a87ed5f4e518ddfc)

- _(fd)_ fd_mqtt fuzz target; clippy fixes (ww-otj.56) [_(efd9854)_](https://github.com/adamsrnmsu/ferrofire/commit/efd985472d7ed5da2588dce6e88f2d9c9d204b5d)

- _(fd)_ <mqtt-client> session, config and Mosquitto integration tests (ww-otj.56) [_(40bdba6)_](https://github.com/adamsrnmsu/ferrofire/commit/40bdba64f8bb300864b0dca7ca74f52f63900a92)

- _(fd)_ MQTT client engine and Sparkplug B payloads (proto-mqtt, ww-otj.56) [_(8f28e39)_](https://github.com/adamsrnmsu/ferrofire/commit/8f28e39e6729bcf0b2e815abc19c54df0af63951)

- _(fd)_ Modbus/TCP server (<modbus-server>), byte-equal to stock bennu (ww-otj.57) [_(09f2e8c)_](https://github.com/adamsrnmsu/ferrofire/commit/09f2e8c8943ebf6ee0d3c35d140595b82fff4301)

- _(iccp)_ TASE.2 server on the shared MMS layer, with a thin test client (ww-otj.46) [_(2b38a01)_](https://github.com/adamsrnmsu/ferrofire/commit/2b38a01f08624ef875cf06a4742491b7011c562f)

- _(iec61850)_ MMS server and GOOSE publisher from the point map, on a reusable MMS layer (ww-otj.45) [_(d4a820f)_](https://github.com/adamsrnmsu/ferrofire/commit/d4a820fcf31d30e30f8db765586e377cce11b17d)

- _(top)_ ferrofire top, a live ratatui view of tag values, age and staleness with a log pane (ww-otj.39) [_(6e1e49b)_](https://github.com/adamsrnmsu/ferrofire/commit/6e1e49b47c1626623ec65588b9992daec2556a8c)

- _(doctor)_ ferrofire doctor checks provider REP, multicast arrival, group id, seq gaps, bystander flood, clock skew and stale tags with a fix per failure (ww-otj.27) [_(8670715)_](https://github.com/adamsrnmsu/ferrofire/commit/8670715d193487863c92a934548ba723e6217599)

- _(explain)_ ferrofire explain MODEL TAG prints kind, expression, inputs, units, limits offline (ww-otj.29) [_(869d26a)_](https://github.com/adamsrnmsu/ferrofire/commit/869d26a5181192d09684474198aa5fc59922de4d)

- _(probe)_ diff compares RTU Modbus values with the provider over one persistent connection per RTU (ww-otj.28) [_(f1250e7)_](https://github.com/adamsrnmsu/ferrofire/commit/f1250e7296460bd876ec7810ea7e2540b7a8eded)

- _(probe)_ record writes the published values to CSV, one row per cycle with stale flags (ww-otj.26) [_(2f6f44d)_](https://github.com/adamsrnmsu/ferrofire/commit/2f6f44d67d2cf75d9a4ddfeea3202e964a3e7b3e)

- _(probe)_ play runs a script of timed writes (ww-otj.25) [_(62be869)_](https://github.com/adamsrnmsu/ferrofire/commit/62be869199f15d618c4db61aa32926f776c9a450)

- _(probe)_ write type/limit checks (--model), --dry-run; provider rejects out-of-range with ERR naming the limit (ww-otj.24) [_(99493ad)_](https://github.com/adamsrnmsu/ferrofire/commit/99493ad6f56b25135577c3bc8b4c20ff053b8dde)

- _(dnp3)_ outstation module, differential-tested against stock bennu (ww-otj.44) [_(48391da)_](https://github.com/adamsrnmsu/ferrofire/commit/48391dadf403b316a8bfb34645ebaf1a34d157aa)

- _(probe)_ tag globs for read/write/watch expanded against QUERY; tag completion from cached QUERY (ww-otj.23) [_(8e3fe96)_](https://github.com/adamsrnmsu/ferrofire/commit/8e3fe968ba78fafe7f327a086d2b6a47dfa72b79)

- _(probe)_ ferrofire probe query|read|write|watch with --json, write audit line, watch age/stale/seq gaps (ww-otj.21) [_(38e006f)_](https://github.com/adamsrnmsu/ferrofire/commit/38e006f6e5e5961c868d5aec01022d399c3bb2ce)

- _(transport)_ pure-Rust zeromq crate replaces libzmq for REQ/REP; libzmq only a dev-dependency (ww-otj.34) [_(557e3f4)_](https://github.com/adamsrnmsu/ferrofire/commit/557e3f4ae2d1920c8d4d504d7e9a4f9c45d53d90)

- _(conformance)_ testpeers image and acceptance harness, proven on stock bennu DNP3 (ww-otj.43) [_(1aff767)_](https://github.com/adamsrnmsu/ferrofire/commit/1aff767e19270255febcb160fc59c6fc88e525d0)

- _(log)_ ferrofire log status|on|off|level|tail on a running process, idempotent, prints destination (ww-otj.30) [_(c2e1e31)_](https://github.com/adamsrnmsu/ferrofire/commit/c2e1e31b57c98eddf63da9fe40d7dd4c26d5d738)

- _(iccp)_ Zeek (Spicy) analyzer and Wireshark post-dissector for ICCP/TASE.2 (ww-otj.49) [_(548573d)_](https://github.com/adamsrnmsu/ferrofire/commit/548573df804983d5daea2f62701b76673cadd21a)

- _(fd)_ field-device core: config loader, datastore, logic, provider link, module registry (ww-otj.42) [_(1b03809)_](https://github.com/adamsrnmsu/ferrofire/commit/1b03809c5fac7c70b57c56707a15581f9c6cc8d9)

- _(log)_ tracing + tracing-subscriber with a reload level and switchable output (ww-otj.33) [_(98843b5)_](https://github.com/adamsrnmsu/ferrofire/commit/98843b5382896cc8716548843c81241507c70b2d)

- _(cli)_ clap derive CLI for every subcommand, ferrofire completions <shell> (ww-otj.32) [_(bb8746f)_](https://github.com/adamsrnmsu/ferrofire/commit/bb8746f86539cb5670e55d47dcf54ef4fe10cb4b)

- _(datagram)_ per-tag age and Stale quality in Receiver (ww-otj.22) [_(3575058)_](https://github.com/adamsrnmsu/ferrofire/commit/35750588a6d3c08cbc7df0f38bafc3e30e12465a)

- _(bench)_ live waterway-arm64 range runs for datagram v1 (1 h, per-RTU, snooping, faults, pull matrix) (ww-otj.19, ww-2s0) [_(08bf1a3)_](https://github.com/adamsrnmsu/ferrofire/commit/08bf1a33ef120b1e45916398f3ac91b080dd8f3c)

- _(bench)_ datagram v1 client, matrix runner, report and 117-run results (ww-otj.18) [_(aa94617)_](https://github.com/adamsrnmsu/ferrofire/commit/aa94617b58f9adbd8fddb90024d834cc0665ca13)

- _(robust)_ every request gets one reply, softer startup errors, non-blocking UDP, datagram receiver (ww-otj.20) [_(b946592)_](https://github.com/adamsrnmsu/ferrofire/commit/b9465927927aaff06c5f8b39c52b7dfa703aab02)

- _(bench)_ pull-bridge mode (N RTUs x K tags), results and docs (ww-otj.11) [_(7466bcd)_](https://github.com/adamsrnmsu/ferrofire/commit/7466bcd8bcb04eb5bbff947d9446d36c7c5e5191)

- batched READ and pull-bridge subcommand (ww-otj.11) [_(0ae12a8)_](https://github.com/adamsrnmsu/ferrofire/commit/0ae12a87c0bedc2c1cf1d1703034db1e3d456c6b)

- _(bench)_ zmq vs nng benchmark harness (client, packet monitor, orchestrator) [_(705440a)_](https://github.com/adamsrnmsu/ferrofire/commit/705440abc2617b34e5b34431a30502ae7dfa1576)

- deploy recipe for the nng-only image, build-overlay checks the variant [_(53d813c)_](https://github.com/adamsrnmsu/ferrofire/commit/53d813c94e1ec27073d9d7d53192f8b9b22680ea)

- selectable transport, zmq or nng, behind a Transport trait [_(8dad3d0)_](https://github.com/adamsrnmsu/ferrofire/commit/8dad3d004435c97a74ee876480b7412ade5cb129)

### 🐛 Bug Fixes

- _(mms)_ parameterCBB is negotiated down; ferrofire-level nesting/CBB test; fuzz crate carries the patch (ww-otj.61) [_(613885a)_](https://github.com/adamsrnmsu/ferrofire/commit/613885a2c440a9df0e534ab7535dfe2b85724b8f)

- _(mqtt)_ bounded queue cannot livelock, retained commands ignored, NCMD at QoS 1, birth timestamps (ww-otj.56) [_(517475b)_](https://github.com/adamsrnmsu/ferrofire/commit/517475b41f86c2fac2bb9612e2fdbc3e94354651)

- _(parsers)_ a refused read of Supported_Features/TASE2_Version teaches nothing; Zeek log limits (ww-otj.67) [_(996fc87)_](https://github.com/adamsrnmsu/ferrofire/commit/996fc87f20e681565c899178fa1a89fdfea8519e)

- _(iccp)_ 12-bit Supported_Features, TASE2_Version 2000.8 (configurable), calling AP-title in the thin client (ww-otj.62, .63, .66) [_(1b952ac)_](https://github.com/adamsrnmsu/ferrofire/commit/1b952ac59a8ca5518819dae70520808793fe3997)

- _(mms)_ Initiate nesting never above the proposal, object-non-existent is ServiceError access 2 (ww-otj.61) [_(8aa0cdb)_](https://github.com/adamsrnmsu/ferrofire/commit/8aa0cdb0e043da4cac2782404777883fa2e3ca60)

- _(charts)_ 05a y-axis clipped the 22.0 dot; 05b slow-mode range 144-162; 06b note said 10 ms, data says up to 1.4 ms [_(c40c72d)_](https://github.com/adamsrnmsu/ferrofire/commit/c40c72d2efd743f13f2565b16ed2b304f9f44a5e)

- _(log)_ the client times out on a silent process instead of hanging; waterway.yaml comment matches the provider's boolean check [_(a6d5675)_](https://github.com/adamsrnmsu/ferrofire/commit/a6d56751530b189de51843a148a3ca0777a0303e)

- _(iec61850)_ close sockets that never finish the handshake; send a GOOSE state change at once and tick at 1 ms so T1 retransmits are not lumped (ww-otj.45) [_(673093f)_](https://github.com/adamsrnmsu/ferrofire/commit/673093ff74b50c93cbed48b48f62465547d7d0db)

- _(probe)_ play rejects an at: beyond 1e9 s instead of panicking in Duration arithmetic [_(fc0dbae)_](https://github.com/adamsrnmsu/ferrofire/commit/fc0dbaed21012cf1460520accf04f1b38b22cbd2)

- _(datagram)_ receiver tolerates reordered chunks (seq window, per-tag newest cycle) (ww-d4a) [_(1f02594)_](https://github.com/adamsrnmsu/ferrofire/commit/1f02594833dd2677a7fdd2a54734b20d8d04dcd2)

- _(zmq)_ REQ send times out like recv [_(67bd27b)_](https://github.com/adamsrnmsu/ferrofire/commit/67bd27b1745e7ae109e7641a31712e39bd406484)

- _(pull)_ validate tags at startup, rate-limited ERR log, order-preserving dedup, skip missed ticks, cap tags per READ [_(7355fd7)_](https://github.com/adamsrnmsu/ferrofire/commit/7355fd71217580da974b1254f169bebe501b02d3)

- _(nng)_ send each publish cycle as one message; configurable PUB send buffer [_(ebcba0e)_](https://github.com/adamsrnmsu/ferrofire/commit/ebcba0e4df155f5a94cdd3817df285ce8fe192c2)

- _(bench)_ do not count the kernel's multicast loop copy as wire traffic; add report script and README [_(bc10a07)_](https://github.com/adamsrnmsu/ferrofire/commit/bc10a07cee13f9688ea4db3ee1c901f5192f9aed)

- clear clippy warnings (as_chunks, NaN-safe comparisons, char patterns) [_(684196a)_](https://github.com/adamsrnmsu/ferrofire/commit/684196a421327287bb755cb582b920078ea0a11a)

### 🚜 Refactor

- _(scpi)_ split the functions the CRAP gate named; close the gaps mutation testing found [_(85d8f77)_](https://github.com/adamsrnmsu/ferrofire/commit/85d8f779a3bae2815f785d6ba94c070d14a8f725)

- _(fd)_ share the named-point write check and its config parsing between modules [_(bfc3abb)_](https://github.com/adamsrnmsu/ferrofire/commit/bfc3abbbd90b9f686ba869124c2198471d15d127)

- _(mqtt)_ Link and host_port to keep CRAP below 10 (ww-otj.56) [_(7094b1e)_](https://github.com/adamsrnmsu/ferrofire/commit/7094b1e6b0940fc4b6545231ca5385c1e993d9a0)

- _(mqtt)_ split parse/num/write for CRAP < 10, reuse bennu::fmt_value; deny ignores for rumqttc's unused webpki 0.102 (ww-otj.56) [_(e638a6d)_](https://github.com/adamsrnmsu/ferrofire/commit/e638a6dbcfe19c712c3bc4eec86bdacc53e33ed8)

- _(iccp)_ no unreachable branch in the transfer-set timers [_(cddca7f)_](https://github.com/adamsrnmsu/ferrofire/commit/cddca7f054edd2f880ef40b90caf9a5f331beea0)

- confine unsafe to sys and simulink::ipc, deny elsewhere (ww-otj.36) [_(fadc4e4)_](https://github.com/adamsrnmsu/ferrofire/commit/fadc4e401e2d21b8b2c5e8e0378ff6babb84dde8)

- drop the nng transport; seq pseudo-tags, sync publish, per-RTU endpoints (ww-otj.13, .14, .15, .16) [_(5dba478)_](https://github.com/adamsrnmsu/ferrofire/commit/5dba478c3219811bba06264d2a17841fd616449c)

- main dispatches on split_first, Rng::from_seed, daemonize forks before chdir [_(ac2e88e)_](https://github.com/adamsrnmsu/ferrofire/commit/ac2e88e4f3a8b0e65c8e7fd36540105125d8e229)

- share with_nul and usage_exit between providers and the CLI [_(6150f67)_](https://github.com/adamsrnmsu/ferrofire/commit/6150f67eb57cd0e9cff03411b29a06e5efc43e77)

- lower the remaining CC-10 functions; test multicast setup, run args and inline bus types [_(f936ec1)_](https://github.com/adamsrnmsu/ferrofire/commit/f936ec1f0516f61e9d2749d1dafa2ee4da554a6a)

- split the case parsers (py/m matrices, inline rows) into small functions [_(335be54)_](https://github.com/adamsrnmsu/ferrofire/commit/335be541f5579c759cfe425ceaeae74319cc50c1)

- table-driven expression operators and functions; split the lexer and call parser [_(22b9c8a)_](https://github.com/adamsrnmsu/ferrofire/commit/22b9c8aef05a47595fbf7fd88841abcdea7dbbb6)

- split the power-flow solver into one-purpose functions [_(a30e7a2)_](https://github.com/adamsrnmsu/ferrofire/commit/a30e7a2357fe037fbd1ccfa2e6c8163c4c006374)

- make Power tags data (Tab/Flag/F + field tables) instead of two 36-arm matches [_(7492acf)_](https://github.com/adamsrnmsu/ferrofire/commit/7492acf14f1f6b35cbf88a2e163f0121b6a65920)

- split Model::load_in and apply_writes into single-purpose loaders [_(cbe2a3f)_](https://github.com/adamsrnmsu/ferrofire/commit/cbe2a3f730483adf93086487863ebb4c64f0845d)

- split bennu run, pypower cli/reply, check and simulink main; test the binaries end to end [_(ac5d11a)_](https://github.com/adamsrnmsu/ferrofire/commit/ac5d11a5abe9737b8bce14689befd889984f5943)

### 📚 Documentation

- SCPI server and client (config, command reference, testing, plan row, README) [_(ba3119c)_](https://github.com/adamsrnmsu/ferrofire/commit/ba3119c0682a45c2aaaff2caba0644477f118397)

- _(testing)_ MQTT mutation result and conformance numbers (ww-otj.56) [_(2ff867d)_](https://github.com/adamsrnmsu/ferrofire/commit/2ff867dd161db5993e90405e716df6fab9f9cea4)

- _(iccp)_ peers, real captures, what still fails and why; 12-bit features, TASE2_Version, nesting (ww-otj.61, .62, .63, .67) [_(2d8e925)_](https://github.com/adamsrnmsu/ferrofire/commit/2d8e9251d0980296a9efe33bcf3905c27e1bcc77)

- _(mqtt)_ config reference, Sparkplug schema, testing.md section, README and plan status (ww-otj.56) [_(6a149aa)_](https://github.com/adamsrnmsu/ferrofire/commit/6a149aa28e4c6f42e17da07790e25faa4ed6807f)

- _(modbus)_ record stock applying unacknowledged writes ~15 s after the provider goes silent (ww-otj.57) [_(0e10dfb)_](https://github.com/adamsrnmsu/ferrofire/commit/0e10dfb3ecc30ed13362745d0fa7cd4cdc4a603a)

- charts for every comparison, results.md rewritten (what vs what, conditions, unit) [_(49af6ad)_](https://github.com/adamsrnmsu/ferrofire/commit/49af6ad5698e26549eb175944eb56af7a5c9cfab)

- testing strategy per component and protocol, with gaps and ideas (ww-otj.53) [_(cc1f092)_](https://github.com/adamsrnmsu/ferrofire/commit/cc1f09268a141bbd7c4869018595f764bc8d33af)

- ferrofire top screenshot in the README, live tool results in results.md [_(0c8a2e5)_](https://github.com/adamsrnmsu/ferrofire/commit/0c8a2e5f1bd582ad3b0bda8b23f1dc22265c8089)

- _(live-tools)_ live run of probe, doctor, log and top on the waterway range with stock bennu RTUs, captures and scripts (ww-otj.40) [_(39b439f)_](https://github.com/adamsrnmsu/ferrofire/commit/39b439f5703f8de2d2716a16a63fb3b18ec3483b)

- _(iec61850)_ server, GOOSE capabilities, shared MMS layer, spike result, harness notes (ww-otj.45) [_(e520b09)_](https://github.com/adamsrnmsu/ferrofire/commit/e520b095f9e5a4dd3cda0a8dce64c12f10652bea)

- _(probe)_ record and its replay into check (ww-otj.26) [_(b8ed530)_](https://github.com/adamsrnmsu/ferrofire/commit/b8ed530861ff942c7e7f3035dface79d0f7b8cd0)

- field-device protocol plan (parity with stock bennu, tiers, architecture, crates, phases) [_(05156d8)_](https://github.com/adamsrnmsu/ferrofire/commit/05156d8e526a4235211f45dbd47b96c5a1f80c48)

- _(readme)_ explain the three range layers, what bennu is, and what ferrofire replaces [_(cbe96a8)_](https://github.com/adamsrnmsu/ferrofire/commit/cbe96a86ee9881d0f50ab6e7f359b223389e6130)

- results at a glance (live range hour, freshness, loss, faults, fan-out) [_(f48d443)_](https://github.com/adamsrnmsu/ferrofire/commit/f48d443601b8fbe4e6ebc0569b128083fa7feb22)

- _(bench)_ after-ww-d4a table, netem reorder cells re-run with the order-tolerant receiver (ww-d4a) [_(79c8134)_](https://github.com/adamsrnmsu/ferrofire/commit/79c813446fbea3e94f85b9c701f95c4b7cb1ffdd)

- loss is not healed within one period; (1-p)^D model, per-tag staleness, IGMP snooping (ww-d4a) [_(c856707)_](https://github.com/adamsrnmsu/ferrofire/commit/c8567074a575942a6039134faf1f934fb9b06999)

- _(bench)_ live captures go into the repo; mark current live numbers as uncommitted samples; document pull-bridge hardening [_(17aa0b4)_](https://github.com/adamsrnmsu/ferrofire/commit/17aa0b45b68603d3f857a673dd7126f8ca8d84d3)

- _(bench)_ live waterway-arm64 pull-bridge check (ww-otj.10) [_(fdbda43)_](https://github.com/adamsrnmsu/ferrofire/commit/fdbda43bd4dba24ea662e48b7a43963c52946d05)

- _(bench)_ after-fix runs and before/after table for the nng one-message publish (ww-a8n) [_(d60de65)_](https://github.com/adamsrnmsu/ferrofire/commit/d60de655dfcc2eee0aaa30f69aa7836fc9b45321)

- _(bench)_ correct zmq vs nng analysis; show nng latency bimodality per run [_(92bfc45)_](https://github.com/adamsrnmsu/ferrofire/commit/92bfc4587936251d1a15d43ffbd4bc43050dd3ef)

- _(bench)_ zmq vs nng comparison at 10/50/100/800 tags (48 runs, raw data, charts, report) [_(b083c90)_](https://github.com/adamsrnmsu/ferrofire/commit/b083c9068233bd139660845fedb495bb452ce95f)

- record the gates after the transport work [_(16626bb)_](https://github.com/adamsrnmsu/ferrofire/commit/16626bb41efb2982f2623f56a70690f93e240824)

- transport table, frontends table and diagram labels [_(4d9e205)_](https://github.com/adamsrnmsu/ferrofire/commit/4d9e20545f7fa12dda1bc9cc1cb6feb5f0fa9a97)

- record the ub-mode quality pass (before and after) and the real gates in the README [_(ca328d8)_](https://github.com/adamsrnmsu/ferrofire/commit/ca328d8ddf9418f4390487152c9dd91da4ea93ce)

- add ferrofire logo to docs and README [_(9356cf7)_](https://github.com/adamsrnmsu/ferrofire/commit/9356cf786e171de1f3fa455302e4ad9a0f722345)

- rewrite README as bullets with a system diagram; finish rb- to ff- rename [_(220336e)_](https://github.com/adamsrnmsu/ferrofire/commit/220336e8df2f9f8844ed7779e89606c787fe4346)

- field device (bennu-field-deviced drop-in) spec + stock conformance probes [_(bb5675b)_](https://github.com/adamsrnmsu/ferrofire/commit/bb5675bd5908867f15f5c97ac1f6925686bb4da1)

- add README with name and tagline [_(9ee9271)_](https://github.com/adamsrnmsu/ferrofire/commit/9ee9271f6de16d037d6c64bbfee7acdc765d06a1)

### 🎨 Styling

- cargo fmt [_(ab8c4f4)_](https://github.com/adamsrnmsu/ferrofire/commit/ab8c4f410fd7144638d09b6b96b0488ab0969abc)

### 🧪 Testing

- _(scpi)_ the client against a foreign CRLF responder [_(71aeced)_](https://github.com/adamsrnmsu/ferrofire/commit/71aeced91d5fda90bf913359a2da510b1bc6d683)

- _(conformance)_ SCPI harness with PyVISA, lxi-tools and ferrofire scpi --check [_(361f50d)_](https://github.com/adamsrnmsu/ferrofire/commit/361f50ddec8afef3abb718bdbbb953e606f97e9c)

- _(scpi)_ transcript golden, socket tests and fuzz target fd_scpi [_(e8356a5)_](https://github.com/adamsrnmsu/ferrofire/commit/e8356a5d59cb1ab3d67044089e36267f3bc2ad91)

- _(conformance)_ independent peers for IEC 61850 and DNP3, public capture checks, peer search results (ww-otj.55, ww-otj.72) [_(e774719)_](https://github.com/adamsrnmsu/ferrofire/commit/e774719c7a719ec7b6a902af597dc61d104fcc0b)

- _(mms)_ nesting negotiation over proposals below, at and above ours; peer-limit xfails name ww-c43 [_(28a1b57)_](https://github.com/adamsrnmsu/ferrofire/commit/28a1b573104f13b95e9853ee9be3b30fe2118219)

- _(mqtt)_ a broker certificate from another CA is refused; document the oversize retained-command limit (ww-otj.56) [_(5ec2dfd)_](https://github.com/adamsrnmsu/ferrofire/commit/5ec2dfdf1bb473ccf1f58583ad8ba182a8b0e9f9)

- _(mqtt)_ clean session, prompt stop, unreadable TLS files; unix_ms and retained branch simplified (ww-otj.56) [_(60478f5)_](https://github.com/adamsrnmsu/ferrofire/commit/60478f5ddb55df466d4a1650980e248b29ad8da5)

- _(iccp)_ independent peers in the harness, both directions, and the real captures (ww-otj.67) [_(ab6bc2c)_](https://github.com/adamsrnmsu/ferrofire/commit/ab6bc2c93884384bb26eab11ca3b0537b0808a6d)

- _(conformance)_ PROTO=mqtt with Mosquitto, Paho, Tahu host, tshark and Zeek (ww-otj.56) [_(7e3d603)_](https://github.com/adamsrnmsu/ferrofire/commit/7e3d603300de56dceb8b40c356b9f769cf5bc241)

- insta snapshots of probe, doctor, explain, log and top --once output, text and json (ww-otj.38) [_(03e97ee)_](https://github.com/adamsrnmsu/ferrofire/commit/03e97ee4155b9ee8d511c743d8c56750756a6570)

- _(fuzz)_ cargo-fuzz targets for the REP handler, datagram receiver, expr parser and model YAML, with a CI smoke run (ww-otj.37) [_(3f82213)_](https://github.com/adamsrnmsu/ferrofire/commit/3f8221370b92ad4f19f0aa6f9030f89a8b042e6a)

- _(dnp3)_ config parse errors, stock-mode smoke script (ww-otj.44) [_(0dd0790)_](https://github.com/adamsrnmsu/ferrofire/commit/0dd0790c4534418422c3ef0543559e107cd7fa14)

- _(dnp3)_ the stock dnp3-client reads brkr:true and load-power:400.5 from our outstation (SPEC 8.5, ww-otj.44) [_(220c023)_](https://github.com/adamsrnmsu/ferrofire/commit/220c0236abb4e451ae6167f6eea67178808e2314)

- _(dnp3)_ harness config with a direct analog output; SPEC 4.2.1 findings; README for differential and DUT=rust (ww-otj.44) [_(a50f831)_](https://github.com/adamsrnmsu/ferrofire/commit/a50f83189c6eaaa1c629584b060ac9f04dd12f9b)

- _(fd)_ robustness tests, fuzz targets, crap/dry refactors (ww-otj.42) [_(4704512)_](https://github.com/adamsrnmsu/ferrofire/commit/4704512ba804bc4025ea506b21bbccdf91a04339)

- _(pull)_ nng-only variant needs a tcp publish endpoint in the unknown-tag test [_(6cb36cb)_](https://github.com/adamsrnmsu/ferrofire/commit/6cb36cbd8cfa5e9e811ff50d21b4f1325679fc86)

- make the IPC and daemon tests safe under concurrent cargo test runs; pin the write tolerance [_(4d980f5)_](https://github.com/adamsrnmsu/ferrofire/commit/4d980f5742a3e8dad84fa307baca25ce508c96e2)

- cover the last uncovered mutation sites; parent of daemonize exits via exit() so its coverage is recorded [_(6fdaff1)_](https://github.com/adamsrnmsu/ferrofire/commit/6fdaff1b75257631547661a4d3e0e3cb063bbb9a)

### 🧹 Chore

- untrack __pycache__ and ignore it [_(ca0ffe3)_](https://github.com/adamsrnmsu/ferrofire/commit/ca0ffe3f663b583ac50ba741c52f3dd2c28ca97c)

- prek hooks (hygiene, codespell, shellcheck, zizmor, conventional commits, cargo fmt) [_(960db25)_](https://github.com/adamsrnmsu/ferrofire/commit/960db25e6870d05d1f497ddd6ac38ad730508b91)

### ⏩ CI/CD

- _(release)_ run build and publish when the release job is skipped [_(a9f73fb)_](https://github.com/adamsrnmsu/ferrofire/commit/a9f73fba8cd52e976cbbdf9da308b45541405a90)

- release automation (Releasaurus release PR, static musl binaries, weekly nightly prerelease) [_(3cf1802)_](https://github.com/adamsrnmsu/ferrofire/commit/3cf1802a9a970e0311e7c65bc3e8045a596514e2)

- run make check on ubuntu-24.04-arm [_(2900e61)_](https://github.com/adamsrnmsu/ferrofire/commit/2900e6114c7f264a340dcbae19167f5923d955b5)

### ⚙️ Miscellaneous Tasks

- cargo-deny (licences, advisories, bans) in make check and CI [_(5521cd4)_](https://github.com/adamsrnmsu/ferrofire/commit/5521cd482a945eb18aa717cef6d93b4e565e7bb0)

- pin toolchain (rust-toolchain.toml) and declare rust-version [_(7846dad)_](https://github.com/adamsrnmsu/ferrofire/commit/7846dad9ec128d97faea64f05e32ec98c93be7b6)

- refuse to build or test while make mutate rewrites src/ [_(f779594)_](https://github.com/adamsrnmsu/ferrofire/commit/f7795949434bc05e8de1298b777f60c5a46cf16d)

- make variants and CI build the zmq-only and nng-only binaries; build-linux.sh takes CARGO_FLAGS [_(ad844d3)_](https://github.com/adamsrnmsu/ferrofire/commit/ad844d37a2c3e7d3daac17900aefa8a7c4f0a980)

- add the nng crate and the zmq/nng cargo features, cmake in the ff-rust image [_(0975171)_](https://github.com/adamsrnmsu/ferrofire/commit/0975171ca2660565135a978fc028c73eac5d114f)

- Makefile quality gates and container tooling for ub-mode [_(b6f6dfc)_](https://github.com/adamsrnmsu/ferrofire/commit/b6f6dfce01bcfd62f8c7885cee319229c3d70849)

- Phase 0: engine core, expression parser, waterway as a model file [_(cf47f26)_](https://github.com/adamsrnmsu/ferrofire/commit/cf47f2695e9dc9f1bd605f24f274b2f48fe5b85c)

- Phase 2: native bennu provider (ZMQ REP, UDP RADIO framing, libstdc++ group hash, chunking), rustbelt run, conformance script [_(81e2354)_](https://github.com/adamsrnmsu/ferrofire/commit/81e2354b337157ca865905f624fbe2e257cb8f04)

- Phase 1: AC/DC power flow (PYPOWER port), case parsers, PYPOWER tag vocabulary, power.drive, islands, pump-station model [_(1c1eb24)_](https://github.com/adamsrnmsu/ferrofire/commit/1c1eb248d2a9eb7b755dcc15015c8f551e3eaf43)

- Rename rustbelt to ferrofire (crate, binary, deploy, docs, tests) [_(8a2ef4e)_](https://github.com/adamsrnmsu/ferrofire/commit/8a2ef4e2142a3a64e6ede02fd6853b8efce1afd7)

- Phase 3: pybennu-power-solver drop-in (INI, daemon verbs, pidfile/logs, noise, ACK tag:value), image overlay recipe, conformance vs stock PyPower [_(0456dbe)_](https://github.com/adamsrnmsu/ferrofire/commit/0456dbef713a1d02f77587a4ff07a8dc7f8c803e)
