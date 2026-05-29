.PHONY: e2e e2e-up e2e-down e2e-logs e2e-soak e2e-soak-smoke e2e-soak-kill

e2e:
	$(MAKE) e2e-up
	cd tests/e2e/client && .venv/bin/python3 -m pytest -v
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-up:
	docker compose -f tests/e2e/docker-compose.yml up -d --build postgres redis fake-upstream fake-web
	./tests/e2e/run_migrations.sh
	docker compose -f tests/e2e/docker-compose.yml up -d --build gateway
	./tests/e2e/wait_for_ready.sh

e2e-down:
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-logs:
	docker compose -f tests/e2e/docker-compose.yml logs --tail=200 -f

e2e-soak:
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build postgres redis fake-upstream fake-web
	./tests/e2e/run_migrations.sh
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build gateway
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && .venv/bin/python3 -m pytest -m soak -v --tb=short
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-soak-smoke:
	FAKE_UPSTREAM_CHAOS=off docker compose -f tests/e2e/docker-compose.yml up -d --build postgres redis fake-upstream fake-web
	./tests/e2e/run_migrations.sh
	FAKE_UPSTREAM_CHAOS=off docker compose -f tests/e2e/docker-compose.yml up -d --build gateway
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && SOAK_SMOKE=1 .venv/bin/python3 -m pytest -m soak -k smoke -v
	docker compose -f tests/e2e/docker-compose.yml down -v

# Kill-during-flight durability gate (spec §8.3). Runs the heavy-chaos soak
# while a background loop SIGKILLs the gateway at SOAK_KILL_PROB/s, then
# asserts the dedupe-aware account identity (records + dlq - crashfallback
# dups == upstream hits). The gateway compose service has `restart:
# unless-stopped` so docker brings it back after each kill.
e2e-soak-kill:
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build postgres redis fake-upstream fake-web
	./tests/e2e/run_migrations.sh
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build gateway
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && SOAK_KILL_PROB=0.1 .venv/bin/python3 -m pytest -m soak -k chaos_kill -v --tb=short
	docker compose -f tests/e2e/docker-compose.yml down -v
