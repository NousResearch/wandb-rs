use graphql_client::GraphQLQuery;

type JSONString = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/mutation_run.graphql",
    skip_serializing_none,
    response_derives = "Debug"
)]
pub struct UpsertBucket;
