terraform {
  required_version = ">= 1.5.0"
  required_providers {
    azurerm = { source = "hashicorp/azurerm", version = "~> 3.110" }
    helm    = { source = "hashicorp/helm",    version = "~> 2.13" }
  }
}

variable "subscription_id" { type = string }
variable "location"        { type = string default = "eastus" }
variable "cluster_name"    { type = string default = "cognitora" }
variable "vm_size"         { type = string default = "Standard_NC8as_T4_v3" }
variable "node_count"      { type = number default = 2 }

provider "azurerm" {
  features {}
  subscription_id = var.subscription_id
}

resource "azurerm_resource_group" "this" {
  name     = "${var.cluster_name}-rg"
  location = var.location
}

resource "azurerm_kubernetes_cluster" "this" {
  name                = var.cluster_name
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  dns_prefix          = var.cluster_name

  default_node_pool {
    name       = "default"
    vm_size    = "Standard_DS2_v2"
    node_count = 1
  }
  identity { type = "SystemAssigned" }
}

resource "azurerm_kubernetes_cluster_node_pool" "gpu" {
  name                  = "gpu"
  kubernetes_cluster_id = azurerm_kubernetes_cluster.this.id
  vm_size               = var.vm_size
  node_count            = var.node_count
  node_taints           = ["nvidia.com/gpu=true:NoSchedule"]
  node_labels           = { "nvidia.com/gpu.present" = "true" }
}

provider "helm" {
  kubernetes {
    host = azurerm_kubernetes_cluster.this.kube_config.0.host
    cluster_ca_certificate = base64decode(azurerm_kubernetes_cluster.this.kube_config.0.cluster_ca_certificate)
    client_certificate     = base64decode(azurerm_kubernetes_cluster.this.kube_config.0.client_certificate)
    client_key             = base64decode(azurerm_kubernetes_cluster.this.kube_config.0.client_key)
  }
}

resource "helm_release" "cognitora" {
  name       = "cognitora"
  repository = "../../kubernetes/helm"
  chart      = "cognitora"
  namespace  = "cognitora"
  create_namespace = true
}

output "cluster_fqdn" { value = azurerm_kubernetes_cluster.this.fqdn }
