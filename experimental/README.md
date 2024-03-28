# Shared library experiments and POC

In past a lot of interesting projects have been kept eternally on a branch of libdatadog.

However this has made collaboration and reuse of code within core of libdatadog harder. 

As a consequences, we've discussed in the past the idea of having a semi-temporary experiments sub project within libdatadog. Where all those not yet-production ready (or meant for production) projects can live, and facilitiate code reuse and better collaboration based on Pull requests.

## Rules for experiments 

For now *by default* all of the projects withing `experimental` namespace should not be build alongside the full libdatadog crate, to avoid unnecessary increase of build times and the complexity of the libdatadog's CI.

Experiments are free to add CI configuration as they see fit. They should however prefer to constrain the CI run only to their own folders, e.g.

```
name: Experimental CI
on:
  push:
    paths:
      - 'experimental/custom-parsing/**' # Only run action when custom parsing code has changes

```

To simplify the experiment setup - an experimental Cargo workspace will be created - experiments are free to add themselves to the workspace - however its not mandatory. The workspace purpose will mostly be relegeated to CI automations.


