// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Comprehensive cluster health status command

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::Client;
use kube::api::{Api, ListParams};
use serde::Serialize;

use super::kube_client;
use super::state::{ClusterState, NodeRole};
use super::talos::client::{self, NodeTargetInterceptor};
use super::talos::proto::machine;
use crate::output::json;

#[derive(Args, Clone)]
pub struct HealthArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Only show summary (skip detailed node info)
    #[arg(long, short)]
    pub summary: bool,

    /// Include etcd status (control plane only)
    #[arg(long)]
    pub etcd: bool,

    /// Talosconfig file (defaults to cluster's talosconfig)
    #[arg(long)]
    pub talosconfig: Option<String>,
}

/// Overall cluster health report
#[derive(Debug, Serialize)]
pub struct ClusterHealthReport {
    /// Cluster metadata
    pub cluster: ClusterInfo,

    /// Summary status
    pub summary: HealthSummary,

    /// Per-node health (if not summary mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<NodeHealth>>,

    /// Kubernetes component status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubernetes: Option<KubernetesHealth>,

    /// etcd status (control plane only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etcd: Option<EtcdHealth>,
}

#[derive(Debug, Serialize)]
pub struct ClusterInfo {
    pub name: String,

    pub uuid: String,

    pub created_at: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_plane_endpoint: Option<String>,

    pub total_nodes: usize,

    pub control_plane_nodes: usize,

    pub worker_nodes: usize,
}

#[derive(Debug, Serialize)]
pub struct HealthSummary {
    /// Overall cluster health status
    pub status: HealthStatus,

    /// Number of healthy nodes
    pub healthy_nodes: usize,

    /// Number of unhealthy nodes
    pub unhealthy_nodes: usize,

    /// Number of unreachable nodes
    pub unreachable_nodes: usize,

    /// Brief description of any issues
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Degraded => write!(f, "degraded"),
            HealthStatus::Unhealthy => write!(f, "unhealthy"),
            HealthStatus::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct NodeHealth {
    /// Node name
    pub name: String,

    /// Node role
    pub role: String,

    /// IP address used to reach the node
    pub endpoint: String,

    /// Whether the node is reachable via Talos API
    pub reachable: bool,

    /// Talos version (if reachable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talos_version: Option<String>,

    /// Kubernetes node status (Ready/NotReady)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k8s_status: Option<String>,

    /// Talos service statuses
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceStatus>,

    /// Issues detected on this node
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    /// Service ID (e.g. "kubelet", "etcd", "containerd")
    pub id: String,

    /// Service state (e.g. "Running", "Finished")
    pub state: String,

    /// Health status
    pub healthy: bool,

    /// Last health message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KubernetesHealth {
    /// Whether the Kubernetes API is reachable
    pub api_reachable: bool,

    /// Kubernetes version (from server)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Node statuses from Kubernetes
    pub nodes: Vec<K8sNodeStatus>,

    /// System pod statuses
    pub system_pods: Vec<K8sPodStatus>,
}

#[derive(Debug, Serialize)]
pub struct K8sNodeStatus {
    pub name: String,

    pub status: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubelet_version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct K8sPodStatus {
    pub name: String,

    pub namespace: String,

    pub status: String,

    pub ready: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EtcdHealth {
    /// Whether etcd cluster is healthy
    pub healthy: bool,

    /// Number of etcd members
    pub member_count: usize,

    /// etcd members
    pub members: Vec<EtcdMemberInfo>,

    /// Alarms (if any)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alarms: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct EtcdMemberInfo {
    pub id: String,

    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_url: Option<String>,

    pub is_learner: bool,
}

pub async fn run(args: HealthArgs, use_json: bool) -> Result<()> {
    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    let cluster_dir = cluster.cluster_dir()?;
    let kubeconfig_path = cluster_dir.join("kubeconfig");
    let talosconfig_path = args
        .talosconfig
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| cluster_dir.join("talosconfig"));

    // Check if cluster has been bootstrapped
    let control_endpoint = cluster
        .control_plane
        .as_ref()
        .and_then(|cp| cp.endpoint.as_ref())
        .cloned();

    let Some(control_endpoint) = control_endpoint else {
        bail!(
            "Cluster '{}' has not been bootstrapped. Run 'triton k8s bootstrap' first.",
            cluster.name
        );
    };
    let talosconfig_str = talosconfig_path.to_string_lossy().to_string();

    // Gather cluster info
    let control_nodes: Vec<_> = cluster
        .nodes
        .iter()
        .filter(|(_, n)| n.role == NodeRole::Control)
        .collect();
    let worker_nodes: Vec<_> = cluster
        .nodes
        .iter()
        .filter(|(_, n)| n.role == NodeRole::Worker)
        .collect();

    let cluster_info = ClusterInfo {
        name: cluster.name.clone(),
        uuid: cluster.uuid.to_string(),
        created_at: cluster.created_at.to_rfc3339(),
        control_plane_endpoint: Some(control_endpoint.clone()),
        total_nodes: cluster.nodes.len(),
        control_plane_nodes: control_nodes.len(),
        worker_nodes: worker_nodes.len(),
    };

    // Collect node health
    let mut node_healths = Vec::new();
    let mut healthy_count = 0usize;
    let mut unhealthy_count = 0usize;
    let mut unreachable_count = 0usize;
    let mut issues = Vec::new();

    // Build a map of k8s node statuses if kubeconfig exists
    let k8s_node_map = if kubeconfig_path.exists() {
        get_k8s_node_map(&kubeconfig_path).await.unwrap_or_default()
    } else {
        HashMap::new()
    };

    for (name, info) in &cluster.nodes {
        let (endpoint, target_node) = match info.role {
            NodeRole::Control => {
                let ep = info
                    .primary_ip
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Control node {} has no primary IP", name))?;
                (ep.clone(), None)
            }
            NodeRole::Worker => {
                let fabric_ip = info
                    .fabric_ip
                    .as_ref()
                    .or(info.primary_ip.as_ref())
                    .ok_or_else(|| anyhow::anyhow!("Worker node {} has no IP address", name))?;
                (control_endpoint.clone(), Some(fabric_ip.as_str()))
            }
        };

        let node_health = query_node_health(
            name,
            info.role,
            &endpoint,
            target_node,
            &talosconfig_str,
            k8s_node_map.get(name).cloned(),
        )
        .await;

        if node_health.reachable {
            if node_health.issues.is_empty() {
                healthy_count += 1;
            } else {
                unhealthy_count += 1;
                for issue in &node_health.issues {
                    issues.push(format!("{}: {}", name, issue));
                }
            }
        } else {
            unreachable_count += 1;
            issues.push(format!("{}: unreachable", name));
        }

        node_healths.push(node_health);
    }

    // Determine overall status
    let status = if unreachable_count > 0 || unhealthy_count > 0 {
        if healthy_count > 0 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Unhealthy
        }
    } else if healthy_count > 0 {
        HealthStatus::Healthy
    } else {
        HealthStatus::Unknown
    };

    let summary = HealthSummary {
        status,
        healthy_nodes: healthy_count,
        unhealthy_nodes: unhealthy_count,
        unreachable_nodes: unreachable_count,
        issues,
    };

    // Get Kubernetes health if kubeconfig exists
    let kubernetes = if kubeconfig_path.exists() {
        get_kubernetes_health(&kubeconfig_path).await.ok()
    } else {
        None
    };

    // Get etcd health if requested
    let etcd = if args.etcd && !control_nodes.is_empty() {
        get_etcd_health(&control_endpoint, &talosconfig_str)
            .await
            .ok()
    } else {
        None
    };

    let report = ClusterHealthReport {
        cluster: cluster_info,
        summary,
        nodes: if args.summary {
            None
        } else {
            Some(node_healths)
        },
        kubernetes,
        etcd,
    };

    if use_json {
        json::print_json(&report)?;
    } else {
        print_health_report(&report);
    }

    Ok(())
}

async fn query_node_health(
    name: &str,
    role: NodeRole,
    endpoint: &str,
    target_node: Option<&str>,
    talosconfig: &str,
    k8s_status: Option<String>,
) -> NodeHealth {
    let role_str = match role {
        NodeRole::Control => "control",
        NodeRole::Worker => "worker",
    };

    let display_endpoint = target_node.unwrap_or(endpoint).to_string();

    // Try to connect and get service list
    let services_result = get_talos_services(endpoint, target_node, talosconfig).await;

    match services_result {
        Ok((services, version)) => {
            let mut issues = Vec::new();

            // Check for unhealthy critical services
            // Non-critical services (dashboard, etc.) don't affect overall health
            let critical_services = [
                "apid",
                "machined",
                "containerd",
                "kubelet",
                "etcd",
                "trustd",
            ];
            for svc in &services {
                if !svc.healthy
                    && svc.state == "Running"
                    && critical_services.contains(&svc.id.as_str())
                {
                    issues.push(format!(
                        "service {} unhealthy: {}",
                        svc.id,
                        svc.message.as_deref().unwrap_or("unknown")
                    ));
                }
            }

            // Check k8s status
            if let Some(ref status) = k8s_status
                && status != "Ready"
            {
                issues.push(format!("Kubernetes node status: {}", status));
            }

            NodeHealth {
                name: name.to_string(),
                role: role_str.to_string(),
                endpoint: display_endpoint,
                reachable: true,
                talos_version: Some(version),
                k8s_status,
                services,
                issues,
            }
        }
        Err(_) => NodeHealth {
            name: name.to_string(),
            role: role_str.to_string(),
            endpoint: display_endpoint,
            reachable: false,
            talos_version: None,
            k8s_status,
            services: vec![],
            issues: vec!["Cannot reach Talos API".to_string()],
        },
    }
}

async fn get_talos_services(
    endpoint: &str,
    target_node: Option<&str>,
    talosconfig: &str,
) -> Result<(Vec<ServiceStatus>, String)> {
    let channel = client::connect(endpoint, Some(talosconfig), false)
        .await
        .context("connecting to Talos API")?;

    // Get service list
    let service_response = if let Some(target) = target_node {
        let interceptor = NodeTargetInterceptor::new(&[target]);
        let mut client = machine::machine_service_client::MachineServiceClient::with_interceptor(
            channel.clone(),
            interceptor,
        );
        client
            .service_list(())
            .await
            .context("listing services via proxy")?
            .into_inner()
    } else {
        let mut client =
            machine::machine_service_client::MachineServiceClient::new(channel.clone());
        client
            .service_list(())
            .await
            .context("listing services")?
            .into_inner()
    };

    let services_msg = service_response
        .messages
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no service list response"))?;

    let services: Vec<ServiceStatus> = services_msg
        .services
        .into_iter()
        .map(|svc| {
            let health = svc.health.as_ref();
            ServiceStatus {
                id: svc.id,
                state: svc.state,
                healthy: health.is_some_and(|h| h.healthy),
                message: health.and_then(|h| {
                    if h.last_message.is_empty() {
                        None
                    } else {
                        Some(h.last_message.clone())
                    }
                }),
            }
        })
        .collect();

    // Get version
    let version_response = if let Some(target) = target_node {
        let interceptor = NodeTargetInterceptor::new(&[target]);
        let mut client = machine::machine_service_client::MachineServiceClient::with_interceptor(
            channel,
            interceptor,
        );
        client
            .version(())
            .await
            .context("getting version via proxy")?
            .into_inner()
    } else {
        let mut client = machine::machine_service_client::MachineServiceClient::new(channel);
        client
            .version(())
            .await
            .context("getting version")?
            .into_inner()
    };

    let version = version_response
        .messages
        .into_iter()
        .next()
        .and_then(|m| m.version)
        .map(|v| v.tag)
        .unwrap_or_else(|| "unknown".to_string());

    Ok((services, version))
}

async fn get_k8s_node_map(kubeconfig_path: &Path) -> Result<HashMap<String, String>> {
    let client = kube_client::client_from_kubeconfig(kubeconfig_path).await?;
    let nodes: Api<Node> = Api::all(client);
    let node_list = nodes.list(&ListParams::default()).await?;

    let mut map = HashMap::new();
    for node in node_list.items {
        let name = node.metadata.name.unwrap_or_default();
        let status = node
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .and_then(|conditions| {
                conditions.iter().find(|c| c.type_ == "Ready").map(|c| {
                    if c.status == "True" {
                        "Ready".to_string()
                    } else {
                        "NotReady".to_string()
                    }
                })
            })
            .unwrap_or_else(|| "Unknown".to_string());
        map.insert(name, status);
    }

    Ok(map)
}

async fn get_kubernetes_health(kubeconfig_path: &Path) -> Result<KubernetesHealth> {
    let client = kube_client::client_from_kubeconfig(kubeconfig_path).await?;

    // Get nodes
    let nodes_api: Api<Node> = Api::all(client.clone());
    let node_list = nodes_api.list(&ListParams::default()).await?;

    let nodes: Vec<K8sNodeStatus> = node_list
        .items
        .iter()
        .map(|node| {
            let name = node.metadata.name.clone().unwrap_or_default();
            let status = node
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .and_then(|conditions| {
                    conditions.iter().find(|c| c.type_ == "Ready").map(|c| {
                        if c.status == "True" {
                            "Ready".to_string()
                        } else {
                            format!("NotReady ({})", c.reason.as_deref().unwrap_or("unknown"))
                        }
                    })
                })
                .unwrap_or_else(|| "Unknown".to_string());
            let kubelet_version = node
                .status
                .as_ref()
                .and_then(|s| s.node_info.as_ref())
                .map(|i| i.kubelet_version.clone());
            let internal_ip = node
                .status
                .as_ref()
                .and_then(|s| s.addresses.as_ref())
                .and_then(|addrs| {
                    addrs
                        .iter()
                        .find(|a| a.type_ == "InternalIP")
                        .map(|a| a.address.clone())
                });

            K8sNodeStatus {
                name,
                status,
                kubelet_version,
                internal_ip,
            }
        })
        .collect();

    // Get system pods (kube-system namespace)
    let pods_api: Api<Pod> = Api::namespaced(client.clone(), "kube-system");
    let pod_list = pods_api.list(&ListParams::default()).await?;

    let system_pods: Vec<K8sPodStatus> = pod_list
        .items
        .iter()
        .map(|pod| {
            let name = pod.metadata.name.clone().unwrap_or_default();
            let status = pod
                .status
                .as_ref()
                .and_then(|s| s.phase.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            let ready = pod
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True")
                });
            let node = pod.spec.as_ref().and_then(|s| s.node_name.clone());

            K8sPodStatus {
                name,
                namespace: "kube-system".to_string(),
                status,
                ready,
                node,
            }
        })
        .collect();

    // Try to get version
    let version = get_k8s_version(&client).await.ok();

    Ok(KubernetesHealth {
        api_reachable: true,
        version,
        nodes,
        system_pods,
    })
}

async fn get_k8s_version(client: &Client) -> Result<String> {
    let version = client.apiserver_version().await?;
    Ok(format!("{}.{}", version.major, version.minor))
}

async fn get_etcd_health(endpoint: &str, talosconfig: &str) -> Result<EtcdHealth> {
    let channel = client::connect(endpoint, Some(talosconfig), false).await?;
    let mut client = machine::machine_service_client::MachineServiceClient::new(channel.clone());

    // Get etcd member list
    let member_response = client
        .etcd_member_list(machine::EtcdMemberListRequest { query_local: false })
        .await
        .context("listing etcd members")?
        .into_inner();

    let members_msg = member_response
        .messages
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no etcd member list response"))?;

    let members: Vec<EtcdMemberInfo> = members_msg
        .members
        .into_iter()
        .map(|m| EtcdMemberInfo {
            id: format!("{:x}", m.id),
            name: m.hostname,
            client_url: m.client_urls.into_iter().next(),
            is_learner: m.is_learner,
        })
        .collect();

    // Get etcd alarms
    let mut alarm_client = machine::machine_service_client::MachineServiceClient::new(channel);
    let alarm_response = alarm_client.etcd_alarm_list(()).await;

    let alarms: Vec<String> = alarm_response
        .ok()
        .and_then(|r| r.into_inner().messages.into_iter().next())
        .map(|m| {
            m.member_alarms
                .into_iter()
                .filter(|a| a.alarm != 0) // NONE = 0
                .map(|a| {
                    let alarm_type = match a.alarm {
                        1 => "NOSPACE",
                        2 => "CORRUPT",
                        _ => "UNKNOWN",
                    };
                    format!("member {:x}: {}", a.member_id, alarm_type)
                })
                .collect()
        })
        .unwrap_or_default();

    let healthy = !members.is_empty() && alarms.is_empty();

    Ok(EtcdHealth {
        healthy,
        member_count: members.len(),
        members,
        alarms,
    })
}

fn print_health_report(report: &ClusterHealthReport) {
    // Header
    eprintln!("Cluster Health Report");
    eprintln!("=====================");
    eprintln!();

    // Cluster info
    eprintln!(
        "Cluster:    {} ({})",
        report.cluster.name,
        &report.cluster.uuid[..8]
    );
    if let Some(ref endpoint) = report.cluster.control_plane_endpoint {
        eprintln!("Endpoint:   {}", endpoint);
    }
    eprintln!(
        "Nodes:      {} total ({} control, {} workers)",
        report.cluster.total_nodes, report.cluster.control_plane_nodes, report.cluster.worker_nodes
    );
    eprintln!();

    // Summary
    let status_display = match report.summary.status {
        HealthStatus::Healthy => "\x1b[32mhealthy\x1b[0m",
        HealthStatus::Degraded => "\x1b[33mdegraded\x1b[0m",
        HealthStatus::Unhealthy => "\x1b[31munhealthy\x1b[0m",
        HealthStatus::Unknown => "\x1b[90munknown\x1b[0m",
    };
    eprintln!("Status:     {}", status_display);
    eprintln!(
        "Healthy:    {}/{}",
        report.summary.healthy_nodes,
        report.summary.healthy_nodes
            + report.summary.unhealthy_nodes
            + report.summary.unreachable_nodes
    );
    if !report.summary.issues.is_empty() {
        eprintln!();
        eprintln!("Issues:");
        for issue in &report.summary.issues {
            eprintln!("  - {}", issue);
        }
    }

    // Node details
    if let Some(ref nodes) = report.nodes {
        eprintln!();
        eprintln!("Node Health");
        eprintln!("-----------");
        for node in nodes {
            let status_icon = if node.reachable && node.issues.is_empty() {
                "\x1b[32m✓\x1b[0m"
            } else if node.reachable {
                "\x1b[33m!\x1b[0m"
            } else {
                "\x1b[31m✗\x1b[0m"
            };

            let version_str = node
                .talos_version
                .as_ref()
                .map(|v| format!(" ({})", v))
                .unwrap_or_default();

            eprintln!(
                "{} {} [{}] {}{}",
                status_icon, node.name, node.role, node.endpoint, version_str
            );

            if let Some(ref k8s_status) = node.k8s_status {
                eprintln!("    K8s: {}", k8s_status);
            }

            // Show key services
            let key_services: Vec<_> = node
                .services
                .iter()
                .filter(|s| {
                    matches!(
                        s.id.as_str(),
                        "kubelet" | "etcd" | "containerd" | "apid" | "machined"
                    )
                })
                .collect();

            if !key_services.is_empty() {
                let svc_status: Vec<_> = key_services
                    .iter()
                    .map(|s| {
                        let icon = if s.healthy { "✓" } else { "✗" };
                        format!("{}:{}", s.id, icon)
                    })
                    .collect();
                eprintln!("    Services: {}", svc_status.join(" "));
            }
        }
    }

    // Kubernetes health
    if let Some(ref k8s) = report.kubernetes {
        eprintln!();
        eprintln!("Kubernetes");
        eprintln!("----------");
        let api_status = if k8s.api_reachable {
            "\x1b[32mreachable\x1b[0m"
        } else {
            "\x1b[31munreachable\x1b[0m"
        };
        eprintln!("API:        {}", api_status);
        if let Some(ref version) = k8s.version {
            eprintln!("Version:    {}", version);
        }

        // Show system pods summary
        let running = k8s.system_pods.iter().filter(|p| p.ready).count();
        let total = k8s.system_pods.len();
        eprintln!("System Pods: {}/{} ready", running, total);

        // Show unhealthy pods
        let unhealthy: Vec<_> = k8s.system_pods.iter().filter(|p| !p.ready).collect();
        if !unhealthy.is_empty() {
            eprintln!("  Not ready:");
            for pod in unhealthy {
                eprintln!("    - {} ({})", pod.name, pod.status);
            }
        }
    }

    // etcd health
    if let Some(ref etcd) = report.etcd {
        eprintln!();
        eprintln!("etcd");
        eprintln!("----");
        let status = if etcd.healthy {
            "\x1b[32mhealthy\x1b[0m"
        } else {
            "\x1b[31munhealthy\x1b[0m"
        };
        eprintln!("Status:     {}", status);
        eprintln!("Members:    {}", etcd.member_count);
        for member in &etcd.members {
            let learner = if member.is_learner { " (learner)" } else { "" };
            eprintln!("  - {} [{}]{}", member.name, member.id, learner);
        }
        if !etcd.alarms.is_empty() {
            eprintln!("Alarms:");
            for alarm in &etcd.alarms {
                eprintln!("  - {}", alarm);
            }
        }
    }
}
