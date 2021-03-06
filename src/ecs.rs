use std::collections::HashMap;
use std::sync::Arc;
use std::vec::Vec;

use crate::client::HttpClient;
use crate::credentials::Credentials;
use anyhow::Result;

use futures::future::join_all;

use rusoto_core::region::Region;
use rusoto_credential::StaticProvider;

use rusoto_ecs::{
    DescribeServicesRequest, DescribeTaskDefinitionRequest, Ecs, EcsClient, ListClustersRequest,
    ListServicesRequest,
};

#[derive(Debug)]
pub struct Image {
    pub image_name: String,
    pub task_definition_name: String,
    pub service_name: String,
}

pub async fn get_image_of_task_definition(
    ecs_client: &EcsClient,
    task_definition: String,
    service_name: String,
) -> Result<Option<Image>> {
    let task_definition_req = DescribeTaskDefinitionRequest {
        task_definition,
        include: None,
    };
    let task_definition_res = ecs_client
        .describe_task_definition(task_definition_req)
        .await?;

    Ok(task_definition_res.task_definition.and_then(move |td| {
        td.clone()
            .task_definition_arn
            .and_then(move |td_arn| {
                td.container_definitions
                    .and_then(|cds| cds.last().map(|cd| (td_arn, cd.clone())))
            })
            .and_then(move |(td_arn, cd)| {
                cd.image.map(|i| Image {
                    image_name: i,
                    task_definition_name: td_arn,
                    service_name,
                })
            })
    }))
}

async fn get_images_of_services(
    ecs_client: &EcsClient,
    service_arns: Vec<String>,
    cluster_name: String,
) -> Result<Vec<Image>> {
    let mut images: Vec<Image> = Vec::new();
    let describe_services_req = DescribeServicesRequest {
        cluster: Some(cluster_name.clone()),
        services: service_arns,
        include: None,
    };

    let describe_services_res = ecs_client.describe_services(describe_services_req).await?;

    if let Some(services) = describe_services_res.services {
        let task_definitions: Vec<(String, String)> = services
            .into_iter()
            .filter_map(|service| {
                service
                    .clone()
                    .task_definition
                    .and_then(|td| service.service_name.map(|sn| (td, sn)))
            })
            .collect();
        let get_images_futures = task_definitions
            .into_iter()
            .map(|(td, sn)| get_image_of_task_definition(ecs_client, td, sn));

        let get_images_results = join_all(get_images_futures).await;

        let get_images_result: Result<Vec<Option<Image>>> =
            get_images_results.into_iter().collect();

        let some_images: Vec<Image> = get_images_result?
            .into_iter()
            .filter_map(|image_opt| image_opt)
            .collect();
        images.extend(some_images);
    }
    Ok(images)
}

pub async fn get_images_of_a_cluster(
    ecs_client: &EcsClient,
    cluster_name: String,
) -> Result<(String, Vec<Image>)> {
    let mut next_token: Option<String> = None;

    let mut all_images: Vec<Image> = Vec::new();

    loop {
        let list_services_req = ListServicesRequest {
            max_results: None,
            next_token,
            cluster: Some(cluster_name.clone()),
            launch_type: None,
            scheduling_strategy: None,
        };

        let list_services_res = ecs_client.list_services(list_services_req).await?;
        if let Some(service_arns) = list_services_res.service_arns {
            if !service_arns.is_empty() {
                let got_images =
                    get_images_of_services(ecs_client, service_arns, cluster_name.clone()).await?;
                all_images.extend(got_images);
            }
        }
        if list_services_res.next_token.is_none() {
            break;
        }
        next_token = list_services_res.next_token;
    }

    Ok((cluster_name.clone(), all_images))
}

pub async fn get_clusters(ecs_client: &EcsClient) -> Result<Vec<String>> {
    let mut clusters: Vec<String> = Vec::new();

    let mut list_clusters_req = ListClustersRequest {
        max_results: None,
        next_token: None,
    };

    loop {
        let list_clusters_res = ecs_client.list_clusters(list_clusters_req.clone()).await?;
        if let Some(cluster_arns) = list_clusters_res.cluster_arns {
            clusters.extend(cluster_arns);
        }

        if list_clusters_res.next_token.is_none() {
            break;
        }
        list_clusters_req.next_token = list_clusters_res.next_token;
    }
    Ok(clusters)
}

pub async fn get_images_of_clusters(
    ecs_client: &EcsClient,
    cluster_includes: &Vec<String>,
) -> Result<HashMap<String, Vec<Image>>> {
    let clusters = get_clusters(ecs_client).await?;
    debug!("Got clusters {:?}", clusters);

    let get_clusters_images_futures = clusters
        .into_iter()
        .filter(|cluster_arn| {
            cluster_includes.is_empty()
                || cluster_includes
                    .iter()
                    .find(|ci| cluster_arn.contains(*ci))
                    .is_some()
        })
        .map(|cluster_arn| get_images_of_a_cluster(ecs_client, cluster_arn));

    let get_clusters_images_res = join_all(get_clusters_images_futures).await;

    let mut res = HashMap::new();
    for cluster_images_res in get_clusters_images_res {
        let cluster_images_tuple = cluster_images_res?;
        let (cluster_name, images) = cluster_images_tuple;
        res.insert(cluster_name, images);
    }

    Ok(res)
}

pub fn build_ecs_client(client: Arc<HttpClient>, creds: Credentials) -> EcsClient {
    let cred_provider = StaticProvider::new(
        creds.aws_access_key,
        creds.aws_secret_key,
        Some(creds.aws_sts_token),
        None,
    );
    EcsClient::new_with(client, cred_provider, Region::EuWest1)
}
