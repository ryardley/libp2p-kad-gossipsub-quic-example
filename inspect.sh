#!/usr/bin/env bash

get_logs_by_version() {
    local SERVICE_NAME=$1
    
    # Get current version number
    CURRENT_VERSION=$(docker service inspect --format '{{.Version.Index}}' $SERVICE_NAME)
    
    # Get all tasks with this version
    TASK_IDS=$(docker service ps --filter "desired-state=running" \
        --format '{{.ID}}' $SERVICE_NAME)
    
    # Get logs from these specific tasks
    for TASK_ID in $TASK_IDS; do
        docker service logs --raw "$TASK_ID"
    done
}

echo ""
echo "================================="
echo "           BOOTSTRAP "
echo "================================="

get_logs_by_version p2p_bootstrap

echo ""
echo "================================="
echo "           PEER "
echo "================================="
get_logs_by_version p2p_peer


echo ""
echo "================================="
echo "           SENDER "
echo "================================="
get_logs_by_version p2p_sender

