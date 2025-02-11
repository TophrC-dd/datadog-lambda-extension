use super::constants::{self, BASE_LAMBDA_INVOCATION_PRICE};
use crate::telemetry::events::ReportMetrics;
use dogstatsd::aggregator::Aggregator;
use dogstatsd::metric;
use dogstatsd::metric::{Metric, MetricValue};
use std::env::consts::ARCH;
use std::sync::{Arc, Mutex};
use tracing::error;

pub struct Lambda {
    pub aggregator: Arc<Mutex<Aggregator>>,
    pub config: Arc<crate::config::Config>,
}

impl Lambda {
    #[must_use]
    pub fn new(aggregator: Arc<Mutex<Aggregator>>, config: Arc<crate::config::Config>) -> Lambda {
        Lambda { aggregator, config }
    }

    pub fn increment_invocation_metric(&self) {
        self.increment_metric(constants::INVOCATIONS_METRIC);
    }

    pub fn increment_errors_metric(&self) {
        self.increment_metric(constants::ERRORS_METRIC);
    }

    pub fn increment_timeout_metric(&self) {
        self.increment_metric(constants::TIMEOUTS_METRIC);
    }

    pub fn set_init_duration_metric(&self, init_duration_ms: f64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            constants::INIT_DURATION_METRIC.into(),
            MetricValue::distribution(init_duration_ms * constants::MS_TO_SEC),
            None,
        );

        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert metric: {}", e);
        }
    }

    fn increment_metric(&self, metric_name: &str) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(metric_name.into(), MetricValue::distribution(1f64), None);
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert metric: {}", e);
        }
    }

    pub fn set_runtime_duration_metric(&self, duration_ms: f64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            constants::RUNTIME_DURATION_METRIC.into(),
            MetricValue::distribution(duration_ms),
            // Datadog expects this value as milliseconds, not seconds
            None,
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert runtime duration metric: {}", e);
        }
    }

    pub fn set_post_runtime_duration_metric(&self, duration_ms: f64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = metric::Metric::new(
            constants::POST_RUNTIME_DURATION_METRIC.into(),
            MetricValue::distribution(duration_ms),
            // Datadog expects this value as milliseconds, not seconds
            None,
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert post runtime duration metric: {}", e);
        }
    }

    fn calculate_estimated_cost_usd(billed_duration_ms: u64, memory_size_mb: u64) -> f64 {
        let gb_seconds = (billed_duration_ms as f64 * constants::MS_TO_SEC)
            * (memory_size_mb as f64 / constants::MB_TO_GB);

        let price_per_gb = match ARCH {
            "x86_64" => constants::X86_LAMBDA_PRICE_PER_GB_SECOND,
            "aarch64" => constants::ARM_LAMBDA_PRICE_PER_GB_SECOND,
            _ => {
                error!("unsupported architecture: {}", ARCH);
                return 0.0;
            }
        };

        ((BASE_LAMBDA_INVOCATION_PRICE + (gb_seconds * price_per_gb)) * 1e12).round() / 1e12
    }

    pub fn set_report_log_metrics(&self, metrics: &ReportMetrics) {
        if !self.config.enhanced_metrics {
            return;
        }
        let mut aggr: std::sync::MutexGuard<Aggregator> =
            self.aggregator.lock().expect("lock poisoned");
        let metric = metric::Metric::new(
            constants::DURATION_METRIC.into(),
            MetricValue::distribution(metrics.duration_ms * constants::MS_TO_SEC),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::BILLED_DURATION_METRIC.into(),
            MetricValue::distribution(metrics.billed_duration_ms as f64 * constants::MS_TO_SEC),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert billed duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MAX_MEMORY_USED_METRIC.into(),
            MetricValue::distribution(metrics.max_memory_used_mb as f64),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert max memory used metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MEMORY_SIZE_METRIC.into(),
            MetricValue::distribution(metrics.memory_size_mb as f64),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert memory size metric: {}", e);
        }

        let cost_usd =
            Self::calculate_estimated_cost_usd(metrics.billed_duration_ms, metrics.memory_size_mb);
        let metric = metric::Metric::new(
            constants::ESTIMATED_COST_METRIC.into(),
            MetricValue::distribution(cost_usd),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert estimated cost metric: {}", e);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config;
    use dogstatsd::metric::EMPTY_TAGS;
    const PRECISION: f64 = 0.000_000_01;

    fn setup() -> (Arc<Mutex<Aggregator>>, Arc<config::Config>) {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        (
            Arc::new(Mutex::new(
                Aggregator::new(EMPTY_TAGS, 1024).expect("failed to create aggregator"),
            )),
            config,
        )
    }

    fn assert_sketch(aggregator_mutex: &Mutex<Aggregator>, metric_id: &str, value: f64) {
        let aggregator = aggregator_mutex.lock().unwrap();
        if let Some(e) = aggregator.get_entry_by_id(metric_id.into(), &None) {
            let metric = e.value.get_sketch().unwrap();
            assert!((metric.max().unwrap() - value).abs() < PRECISION);
            assert!((metric.min().unwrap() - value).abs() < PRECISION);
            assert!((metric.sum().unwrap() - value).abs() < PRECISION);
            assert!((metric.avg().unwrap() - value).abs() < PRECISION);
        } else {
            panic!("{}", format!("{metric_id} not found"));
        }
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_increment_invocation_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_invocation_metric();

        assert_sketch(&metrics_aggr, constants::INVOCATIONS_METRIC, 1f64);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_increment_errors_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_errors_metric();

        assert_sketch(&metrics_aggr, constants::ERRORS_METRIC, 1f64);
    }

    #[test]
    fn test_disabled() {
        let (metrics_aggr, no_config) = setup();
        let my_config = Arc::new(config::Config {
            enhanced_metrics: false,
            ..no_config.as_ref().clone()
        });
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_invocation_metric();
        lambda.increment_errors_metric();
        lambda.increment_timeout_metric();
        lambda.set_init_duration_metric(100.0);
        lambda.set_runtime_duration_metric(100.0);
        lambda.set_post_runtime_duration_metric(100.0);
        lambda.set_report_log_metrics(&ReportMetrics {
            duration_ms: 100.0,
            billed_duration_ms: 100,
            max_memory_used_mb: 128,
            memory_size_mb: 256,
            init_duration_ms: Some(50.0),
            restore_duration_ms: None,
        });
        let aggr = metrics_aggr.lock().expect("lock poisoned");
        assert!(aggr
            .get_entry_by_id(constants::INVOCATIONS_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::ERRORS_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TIMEOUTS_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::INIT_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::RUNTIME_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::POST_RUNTIME_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::BILLED_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::MAX_MEMORY_USED_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::MEMORY_SIZE_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::ESTIMATED_COST_METRIC.into(), &None)
            .is_none());
    }

    #[test]
    fn test_set_report_log_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let report_metrics = ReportMetrics {
            duration_ms: 100.0,
            billed_duration_ms: 100,
            max_memory_used_mb: 128,
            memory_size_mb: 256,
            init_duration_ms: Some(50.0),
            restore_duration_ms: None,
        };
        lambda.set_report_log_metrics(&report_metrics);

        assert_sketch(&metrics_aggr, constants::DURATION_METRIC, 0.1);
        assert_sketch(&metrics_aggr, constants::BILLED_DURATION_METRIC, 0.1);

        assert_sketch(&metrics_aggr, constants::MAX_MEMORY_USED_METRIC, 128.0);
        assert_sketch(&metrics_aggr, constants::MEMORY_SIZE_METRIC, 256.0);
    }
}
