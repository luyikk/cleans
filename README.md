# cargo-cleans
A cargo subcommand cleans all projects under a specified directory with high performance.

## Installation

Install using cargo:
```shell
cargo install cargo-cleans
```

## Usage

Clean all target directories under the current working directory.
```shell
cargo cleans
```

Clean all target directories under the directory `[dir]`.
```shell
cargo cleans -r [dir]
```

Keep target directories that have a size(MB) of less than `[filesize]`.
```shell
cargo cleans --keep-size [filesize(MB)]
```

Keep target directories younger than `[days]` days.
```shell
cargo cleans --keep-days [days]
```

## Compare with other
Use with tokio+actor concurrent search,the performance is 3-5 times that of others.