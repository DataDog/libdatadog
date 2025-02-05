use datadog_protos::metrics::{Metadata, Origin};
use protobuf::MessageField;

const AWS_LAMBDA_PREFIX: &str = "aws.lambda";
const AWS_STEP_FUNCTIONS_PREFIX: &str = "aws.states";

pub enum OriginProduct {
    None,
    Serverless,
    APM,
    Logs,
    Processes,
    RUM,
    Events,
    Synthetics,
    MetricsAPI,
    USM,
    Agent,
    CSM,
    CloudIntegrations,
    APICatalog,
    Vector,
    ObservabilityPipelines,
    DSM,
    DatadogPlatform,
    SAASIntegrations,
    DatadogExporter,
    NPM,
    DBM,
    ServiceCatalog,
    LLMObservability,
    SyncCLI,
    AppBuilder,
    Profiling,
    WorkflowAutomation,
    DQM,
    SoftwareDelivery,
    CloudCostManagement,
    DJM,
    Containers,
    ServiceCheck,
    DatadogOperator,
    SLO,
    StorageMonitoring,
}

impl Into<u32> for OriginProduct {
    fn into(self) -> u32 {
        self as u32
    }
}

pub enum OriginCategory {
    Other,
    Reserved1,
    Traces,
    Spans,
    LogMetrics,
    ProcessMetrics,
    RumMetrics,
    EventMetrics,
    SyntheticsMetrics,
    DistributionMetrics,
    Dogstatsd,
    Integration,
    UsageMetrics,
    Reserved13,
    ApmTraceInternal,
    USMMetrics,
    DSMMetrics,
    OTLP,
    AWS,
    GoogleCloud,
    Azure,
    SAAS,
    DatabaseQueryMetrics,
    DatabaseProcedureMetrics,
    DatabaseLockMetrics,
    DatabaseActivityMetrics,
    ScorecardMetrics,
    LLMObservabilityMetrics,
    Datadogpy,
    DatadogAPIClientPython,
    DatadogAPIClientGo,
    DatadogAPIClientRust,
    DatadogAPIClientJava,
    DatadogAPIClientRuby,
    DatadogAPIClientTypeScript,
    AppServicesMetrics,
    CloudRunMetrics,
    ContainerAppMetrics,
    LambdaMetrics,
    OTLPIntegration,
    OTLPOther,
    StepFunctionsMetrics,
    DatabaseAgentJobsMetrics,
    RateLimiting,
    Accupath,
    ServiceIndex,
    ServiceMap,
    SpanToMetrics,
    TraceMetrics,
    TracerAnalyticsMetrics,
    TracerRuntimeMetrics,
    AppBuilderOOTBDashboard,
    WorkflowAutomationExecutionMetrics,
    DQMMetrics,
    AgentCI,
    CodeCoverage,
    MetricsTagManagement,
    IntegrationHealth,
    PrivateActionsRunner,
    ContainerImages,
    OracleCloudInfrastructure,
    WorkflowAutomationCreationMetrics,
    AmazonS3,
    AzureBlobStorage,
    DatadogAgent,
    DatadogAgentProfile,
    DatadogDashboard,
    DatadogMonitor,
    DatadogSLO,
    GoogleCloudStorage,
    OracleCloudInfrastructureObjectStorage,
}

impl Into<u32> for OriginCategory {
    fn into(self) -> u32 {
        self as u32
    }
}

pub fn get_metric_origin(name: &str) -> Option<Metadata> {
    let prefix = name.split('.').take(2).collect::<Vec<&str>>().join(".");

    match prefix {
        _ if prefix == AWS_LAMBDA_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::LambdaMetrics.into(),
                origin_service: 0, // uncategorized
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ if prefix == AWS_STEP_FUNCTIONS_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::StepFunctionsMetrics.into(),
                origin_service: 0, // uncategorized
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ => None,
    }
}