terraform {
  required_version = ">= 1.5.0"
  required_providers {
    hcloud = { source = "hetznercloud/hcloud", version = "~> 1.49" }
  }
}

variable "hcloud_token" { type = string sensitive = true }
variable "location"     { type = string default = "fsn1" }
variable "router_count" { type = number default = 1 }
variable "gpu_count"    { type = number default = 2 }

provider "hcloud" { token = var.hcloud_token }

# Hetzner doesn't have a managed K8s offering with GPU support today.
# We provision raw GPU servers (CCX series) and let cgn-ctl install
# everything via the install.sh one-liner. This module mirrors what
# bare-metal users do by hand.

resource "hcloud_ssh_key" "this" {
  name       = "cognitora"
  public_key = file("~/.ssh/id_ed25519.pub")
}

resource "hcloud_server" "router" {
  count       = var.router_count
  name        = "cognitora-router-${count.index}"
  server_type = "ccx33"     # 8 vCPU, 32 GB
  image       = "ubuntu-24.04"
  location    = var.location
  ssh_keys    = [hcloud_ssh_key.this.id]

  user_data = <<-EOT
    #!/bin/bash
    set -e
    curl -sSfL https://get.cognitora.dev | sh
    cgn-ctl install single-node --skip-gpu
  EOT
}

resource "hcloud_server" "gpu" {
  count       = var.gpu_count
  name        = "cognitora-gpu-${count.index}"
  server_type = "ccx53"     # placeholder; swap to GPU SKU in your account
  image       = "ubuntu-24.04"
  location    = var.location
  ssh_keys    = [hcloud_ssh_key.this.id]

  user_data = <<-EOT
    #!/bin/bash
    set -e
    apt-get update && apt-get install -y nvidia-driver-535 ca-certificates curl
    curl -sSfL https://get.cognitora.dev | sh
    cgn-ctl install baremetal
  EOT
}

output "router_ips" { value = [for s in hcloud_server.router : s.ipv4_address] }
output "gpu_ips"    { value = [for s in hcloud_server.gpu    : s.ipv4_address] }
