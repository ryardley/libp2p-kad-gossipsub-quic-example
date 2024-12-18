#!/usr/bin/env bash
wait_ready() {
    local stack_name="$1"
    until [ "$(docker stack services $stack_name --format '{{.Replicas}}' | awk -F'/' '$1 != $2')" = "" ]; do
        printf "."
        sleep 1
    done
    echo -ne "\r\033[K"
    echo "Stack $stack_name is ready!"
}

wait_removed() {
  local stack_name="$1"

  while docker stack ps $stack_name >/dev/null 2>&1; do
      printf "."
      sleep 1
  done
  echo -ne "\r\033[K"
  echo "Stack $stack_name is removed"
}

docker stack rm p2p

wait_removed p2p

docker stack deploy -c docker-compose.yml --prune p2p

wait_ready p2p
