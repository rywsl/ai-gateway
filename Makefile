.PHONY: e2e e2e-up e2e-down e2e-logs

e2e:
	docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && python -m pytest -v
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-up:
	docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh

e2e-down:
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-logs:
	docker compose -f tests/e2e/docker-compose.yml logs --tail=200 -f
