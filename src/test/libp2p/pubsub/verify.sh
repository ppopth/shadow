#!/usr/bin/env bash

set -euo pipefail

echo "Test libp2p Ethereum pubsub"

NUM_VALIDATORS=10
NUM_SLOTS=25

### Block proposing

# Verify that the block proposals are done correctly
# Skip the slot 0, because it's the start of the simulation and the proposer probably cannot make it on time
for slot in $(seq 1 $(($NUM_SLOTS - 1))); do
    num_published=0
    proposer_id=
    proposer_msgid=
    for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
        line=$(cat ./hosts/peer$vid/*.stdout | grep "Published to beacon_block .*\"slot\":$slot\>" || true)
        if ! [[ -z "$line" ]]; then
            proposer_id=$vid
            proposer_msgid=$(echo "$line" | grep -o "msgid=[^ ]*" | cut -d '=' -f 2)
            num_published=$(($num_published + 1))
        fi
    done
    if [[ "$num_published" != 1 || -z "$proposer_msgid" ]]; then
        printf "Some slot has zero or more than one proposers"
        exit 1
    fi
    for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
        if [[ $proposer_id == $vid ]]; then
            continue
        fi
        line=$(cat ./hosts/peer$vid/*.stdout | grep "Got block from slot=$slot vid=$proposer_id" || true)
        if [[ -z "$line" ]]; then
            printf "Validator $vid didn't get a block at slot $slot"
            exit 1
        fi
        received_msgid=$(echo "$line" | grep -o "msgid=[^ ]*" | cut -d '=' -f 2)
        if [[ "$received_msgid" != "$proposer_msgid" ]]; then
            printf "Validator $vid received an incorrect block message at slot $slot"
            exit 1
        fi
    done
done

# Verify that each validator receives the correct number of blocks
for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
    published=$(cat ./hosts/peer$vid/*.stdout | grep "Published to beacon_block" | wc -l || true)
    received=$(cat ./hosts/peer$vid/*.stdout | grep "Got block from" | wc -l || true)
    # Skip the slot 0, because it's the start of the simulation and the proposer probably cannot make it on time
    if [[ "$(($published + $received))" != "$(($NUM_SLOTS - 1))" ]]; then
        printf "The number of blocks received and published on validator $vid is incorrect"
        exit 1
    fi
done

echo "Verification succeeded"
