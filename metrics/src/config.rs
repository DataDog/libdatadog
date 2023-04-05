// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#[derive(Debug, Builder)]
#[builder(build_fn(validate = "Self::validate"))]
pub struct Config {
    pub api_key: String,
    #[builder(default = "String::from(\"datadoghq.com\")")]
    pub site: String,
}

impl ConfigBuilder {
    fn validate(&self) -> Result<(), String> {
        let valid_sites = [
            "datadoghq.com",
            "us1.datadoghq.com",
            "us3.datadoghq.com",
            "us5.datadoghq.com",
            "datadoghq.eu",
            "ddog-gov.com",
        ];

        if let Some(ref site) = self.site {
            if !valid_sites.contains(&site.as_str()) {
                return Err(format!("Site {} is not a valid Datadog site", site));
            }
        }

        Ok(())
    }
}
