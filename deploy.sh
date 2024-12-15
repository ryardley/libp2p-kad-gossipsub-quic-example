#!/usr/bin/env bash
docker stack rm p2p && sleep 20
docker stack deploy -c docker-compose.yml --prune p2p && sleep 10
