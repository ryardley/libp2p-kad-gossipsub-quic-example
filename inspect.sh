#!/usr/bin/env bash
echo ""
echo "================================="
echo "           BOOTSTRAP "
echo "================================="

docker service logs p2p_bootstrap

echo ""
echo "================================="
echo "           PEER "
echo "================================="
docker service logs p2p_peer


echo ""
echo "================================="
echo "           SENDER "
echo "================================="
docker service logs p2p_sender

