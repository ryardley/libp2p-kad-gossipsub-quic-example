#!/usr/bin/env bash
wait_for_update() {
    local service=$1
    echo "Waiting for $service to update..."
    while [ "$(docker service inspect --format '{{.UpdateStatus.State}}' $service)" != "completed" ]; do
        sleep 5
    done
    echo "$service update complete"
}


docker service update p2p_bootstrap --force
wait_for_update p2p_bootstrap

docker service update p2p_peer --force
wait_for_update p2p_peer

docker service update p2p_sender --force
wait_for_update p2p_sender
