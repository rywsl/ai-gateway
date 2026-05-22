.PHONY: e2e e2e-up e2e-down e2e-logs e2e-soak e2e-soak-smoke

e2e:
	docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && .venv/bin/python3 -m pytest -v
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-up:
	docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh

e2e-down:
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-logs:
	docker compose -f tests/e2e/docker-compose.yml logs --tail=200 -f

e2e-soak:
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && .venv/bin/python3 -m pytest -m soak -v --tb=short
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-soak-smoke:
	FAKE_UPSTREAM_CHAOS=off docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && SOAK_SMOKE=1 .venv/bin/python3 -m pytest -m soak -k smoke -v
	docker compose -f tests/e2e/docker-compose.yml down -v
