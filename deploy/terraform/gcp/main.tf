terraform {
  required_version = ">= 1.5.0"
  required_providers {
    google = { source = "hashicorp/google", version = "~> 5.0" }
    helm   = { source = "hashicorp/helm",   version = "~> 2.13" }
  }
}

variable "project"      { type = string }
variable "region"       { type = string  default = "us-central1" }
variable "cluster_name" { type = string  default = "cognitora" }
variable "machine_type" { type = string  default = "g2-standard-8" }
variable "gpu_type"     { type = string  default = "nvidia-l4" }
variable "node_count"   { type = number  default = 2 }

provider "google" {
  project = var.project
  region  = var.region
}

resource "google_container_cluster" "this" {
  name             = var.cluster_name
  location         = var.region
  initial_node_count = 1
  remove_default_node_pool = true
  release_channel { channel = "STABLE" }
}

resource "google_container_node_pool" "gpu" {
  name       = "gpu-pool"
  cluster    = google_container_cluster.this.name
  location   = google_container_cluster.this.location
  node_count = var.node_count

  node_config {
    machine_type = var.machine_type
    guest_accelerator {
      type  = var.gpu_type
      count = 1
    }
    labels = { "nvidia.com/gpu.present" = "true" }
    taint {
      key    = "nvidia.com/gpu"
      value  = "true"
      effect = "NO_SCHEDULE"
    }
    oauth_scopes = ["https://www.googleapis.com/auth/cloud-platform"]
  }
}

provider "helm" {
  kubernetes {
    host  = "https://${google_container_cluster.this.endpoint}"
    token = data.google_client_config.this.access_token
    cluster_ca_certificate = base64decode(google_container_cluster.this.master_auth.0.cluster_ca_certificate)
  }
}

data "google_client_config" "this" {}

resource "helm_release" "cognitora" {
  name       = "cognitora"
  repository = "../../kubernetes/helm"
  chart      = "cognitora"
  namespace  = "cognitora"
  create_namespace = true
}

output "cluster_endpoint" { value = google_container_cluster.this.endpoint }
