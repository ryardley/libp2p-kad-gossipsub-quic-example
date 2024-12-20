#!/usr/bin/env bash

# First, flush existing rules
iptables -F
iptables -X
iptables -Z

# Set default policies to DROP
iptables -P INPUT DROP
iptables -P FORWARD DROP
iptables -P OUTPUT ACCEPT

# Allow loopback traffic
iptables -A INPUT -i lo -j ACCEPT
iptables -A OUTPUT -o lo -j ACCEPT

# Allow established and related connections
iptables -A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT

# Allow specific ports (both TCP and UDP)
iptables -A INPUT -p tcp -m multiport --dports 4001,4002,4003 -j ACCEPT
iptables -A INPUT -p udp -m multiport --dports 4001,4002,4003 -j ACCEPT

# If you need SSH access (recommended), add:
iptables -A INPUT -p tcp --dport 22 -j ACCEPT

# For Debian/Ubuntu:
iptables-save > /etc/iptables/rules.v4
# For RHEL/CentOS:
# service iptables save
