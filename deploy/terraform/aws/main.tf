terraform {
  required_version = ">= 1.5.0"
  required_providers {
    aws  = { source = "hashicorp/aws",  version = "~> 5.0" }
    helm = { source = "hashicorp/helm", version = "~> 2.13" }
  }
}

provider "aws" { region = var.region }

variable "region"        { type = string  default = "us-east-1" }
variable "cluster_name"  { type = string  default = "cognitora" }
variable "instance_type" { type = string  default = "g5.2xlarge" }
variable "node_count"    { type = number  default = 2 }

# EKS-managed control plane plus a single GPU nodegroup. The Helm
# release applies the chart from this repository against the cluster.

module "eks" {
  source  = "terraform-aws-modules/eks/aws"
  version = "~> 20.0"

  cluster_name    = var.cluster_name
  cluster_version = "1.30"

  vpc_id                 = aws_vpc.this.id
  subnet_ids             = aws_subnet.private[*].id
  enable_irsa            = true
  manage_aws_auth_configmap = true

  eks_managed_node_groups = {
    gpu = {
      instance_types = [var.instance_type]
      ami_type       = "BOTTLEROCKET_x86_64_NVIDIA"
      min_size       = 1
      max_size       = 10
      desired_size   = var.node_count
      labels = { "nvidia.com/gpu.present" = "true" }
      taints = [{ key = "nvidia.com/gpu", value = "true", effect = "NO_SCHEDULE" }]
    }
  }
}

resource "aws_vpc" "this" {
  cidr_block           = "10.40.0.0/16"
  enable_dns_hostnames = true
  enable_dns_support   = true
  tags = { Name = "${var.cluster_name}-vpc" }
}

resource "aws_subnet" "private" {
  count             = 3
  vpc_id            = aws_vpc.this.id
  cidr_block        = cidrsubnet(aws_vpc.this.cidr_block, 4, count.index)
  availability_zone = data.aws_availability_zones.available.names[count.index]
}

data "aws_availability_zones" "available" { state = "available" }

provider "helm" {
  kubernetes {
    host                   = module.eks.cluster_endpoint
    cluster_ca_certificate = base64decode(module.eks.cluster_certificate_authority_data)
    token                  = data.aws_eks_cluster_auth.this.token
  }
}

data "aws_eks_cluster_auth" "this" { name = module.eks.cluster_name }

resource "helm_release" "cognitora" {
  name       = "cognitora"
  repository = "../../kubernetes/helm"
  chart      = "cognitora"
  namespace  = "cognitora"
  create_namespace = true
  values = [
    yamlencode({
      cluster = { name = var.cluster_name }
    })
  ]
}

output "cluster_endpoint" { value = module.eks.cluster_endpoint }
output "openai_endpoint"  { value = "http://<set after kubectl get svc>:8080" }
