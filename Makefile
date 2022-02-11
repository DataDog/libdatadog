
target:
	@mkdir -p $@

.PHONY: check.licences
check.licences: 
	docker buildx build ./ -f scripts/checks/3rd-party-licenses.Dockerfile --load
	