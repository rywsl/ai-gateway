#!/usr/bin/env ruby
# frozen_string_literal: true

require "pathname"
require "yaml"

def fail_check(message)
  warn "ci image sync check failed: #{message}"
  exit 1
end

def load_ci(path)
  YAML.load_file(path)
rescue StandardError => e
  fail_check("could not parse #{path}: #{e.message}")
end

def script_text(job)
  Array(job.fetch("script")).join("\n")
end

root = Pathname.new(__dir__).join("..").realpath
go_root = Pathname.new(ENV.fetch("AILINKMAX_GO_REPO", root.join("..", "AILinkMax-Go").to_s)).realpath

gateway_ci_path = root.join(".gitlab-ci.yml")
go_ci_path = go_root.join(".gitlab-ci.yml")

gateway_ci = load_ci(gateway_ci_path)
go_ci_text = go_ci_path.read

stages = Array(gateway_ci["stages"])
fail_check("#{gateway_ci_path} must include a deploy stage") unless stages.include?("deploy")

job = gateway_ci["sync-k3s-rust-gateway"]
fail_check("#{gateway_ci_path} must define sync-k3s-rust-gateway") unless job.is_a?(Hash)

needs = Array(job["needs"])
unless needs.any? { |need| need == "push-image" || (need.is_a?(Hash) && need["job"] == "push-image") }
  fail_check("sync-k3s-rust-gateway must run after push-image")
end

sync_script = script_text(job)
[
  'desired_image="$GATEWAY_IMAGE:$CI_COMMIT_SHORT_SHA"',
  'kubectl -n "$K8S_NAMESPACE" get deployment "$GATEWAY_DEPLOYMENT"',
  'jsonpath="{.spec.template.spec.containers[?(@.name==\"$GATEWAY_CONTAINER\")].image}"',
  'if [ "$current_image" = "$desired_image" ]; then',
  'kubectl -n "$K8S_NAMESPACE" set image deployment/"$GATEWAY_DEPLOYMENT" "$GATEWAY_CONTAINER=$desired_image"',
  'kubectl -n "$K8S_NAMESPACE" rollout status deployment/"$GATEWAY_DEPLOYMENT"',
].each do |required|
  fail_check("sync-k3s-rust-gateway is missing #{required.inspect}") unless sync_script.include?(required)
end

[
  "kubectl apply -f",
  "ingress.yaml",
  "statefulset/",
  "deployment/go-allinone",
  "postgres",
  "redis",
  "clickhouse",
].each do |forbidden|
  fail_check("sync-k3s-rust-gateway must not manage #{forbidden}") if sync_script.include?(forbidden)
end

if go_ci_text.include?("rollout restart deployment/rust-gateway")
  fail_check("#{go_ci_path} must not force-restart rust-gateway during every Go deploy")
end

[
  "current_rust_gateway_image=",
  "RUST_GATEWAY_DEPLOY_IMAGE=",
  's#__RUST_GATEWAY_IMAGE__#${RUST_GATEWAY_DEPLOY_IMAGE}#g',
].each do |required|
  fail_check("#{go_ci_path} must preserve the live Rust image before rendering Go-owned manifests") unless go_ci_text.include?(required)
end

puts "ci image sync check ok"
