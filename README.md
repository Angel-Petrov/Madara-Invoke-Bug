https://github.com/Angel-Petrov/Madara-Invoke-Bug/assets/146711006/582c2baa-db8c-4403-97e9-35778cad632e

This repo is a simple project to test a bug in madara.

To run this have madara running locally and just call `cargo run`. Madara versions after commit [`194cf75`](https://github.com/keep-starknet-strange/madara/commit/194cf7547bca0d55bbc5c5f91cd7bcbbb14b63a4) should fail. This issue seems to depend on the amount of times the call is invoked. You can do `cargo run -- 42` to have a custom amount of calls, the default is 1000 which should be enough to lead to the bug occuring.

If you are setting up a madara chain for the first time make sure to let it run for 10 seconds before running this test to avoid getting errors with [simulating tx on block 0 fails](https://github.com/keep-starknet-strange/madara/issues/1443).
