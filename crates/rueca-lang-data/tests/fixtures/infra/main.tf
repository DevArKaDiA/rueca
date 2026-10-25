# Bucket for access logs.
resource "aws_s3_bucket" "logs" {
  bucket = "${var.prefix}-logs"
  tags = {
    env = local.env
  }
}

module "net" {
  source = "./modules/net"
  cidr   = var.cidr
}

data "aws_caller_identity" "current" {}

locals {
  env = "prod"
}

output "bucket_arn" {
  description = "ARN of the log bucket."
  value       = aws_s3_bucket.logs.arn
}
