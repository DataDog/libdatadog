package gitlab

import "encoding/json"
import "encoding/yaml"

import "tool/file"
import "tool/cli"

ParseTargets: {
        #raw: string
        #parsed: json.Unmarshal(#raw) 
        list: [for target, _val in #parsed.target {
            "\(target)"
        }]
}

GitlabPipelines: {
    #targets: [...string]
    include: 
        remote: "https://gitlab-templates.ddbuild.io/libdatadog/include/variables.yml"

    stages: ["check"]

    variables: {
        KUBERNETES_SERVICE_ACCOUNT_OVERWRITE: "libdatadog"
    }

    for check in #targets {       
        (check): {
            tags: [ "runner:docker", "platform:amd64" ]
            stage: "check"
            when: "always"
            image: "${DOCKER_IMAGE}"
            
            script: """
                docker buildx bake \(check) --progress=plain
            """
        }    
    }
}

command:  output: {
    read: file.Read & {
		filename: "./targets.json"
		contents: string
	}
    print: cli.Print & {
		text: yaml.Marshal(GitlabPipelines & { 
            _targets:   ParseTargets & {
                #raw: read.contents
            }
            #targets: _targets.list
        })
	}
}