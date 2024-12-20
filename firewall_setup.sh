#!/usr/bin/env bash

sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
sudo ufw allow 4001/udp
sudo ufw allow 4002/udp
sudo ufw allow 4003/udp

sudo ufw enable
