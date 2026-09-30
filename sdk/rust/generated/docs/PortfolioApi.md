# \PortfolioApi

All URIs are relative to *http://localhost*

Method | HTTP request | Description
------------- | ------------- | -------------
[**calculate_portfolio_payoff_v1**](PortfolioApi.md#calculate_portfolio_payoff_v1) | **POST** /api/v1/portfolio/payoff | 
[**calculate_portfolio_payoff_v2**](PortfolioApi.md#calculate_portfolio_payoff_v2) | **POST** /api/v2/portfolio/payoff | 
[**get_portfolio_greeks_v1**](PortfolioApi.md#get_portfolio_greeks_v1) | **GET** /api/v1/portfolio/greeks | 
[**get_portfolio_greeks_v2**](PortfolioApi.md#get_portfolio_greeks_v2) | **GET** /api/v2/portfolio/greeks | 



## calculate_portfolio_payoff_v1

> std::collections::HashMap<String, serde_json::Value> calculate_portfolio_payoff_v1(request_body)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**request_body** | [**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md) |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## calculate_portfolio_payoff_v2

> std::collections::HashMap<String, serde_json::Value> calculate_portfolio_payoff_v2(request_body)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**request_body** | [**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md) |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_portfolio_greeks_v1

> std::collections::HashMap<String, serde_json::Value> get_portfolio_greeks_v1()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_portfolio_greeks_v2

> std::collections::HashMap<String, serde_json::Value> get_portfolio_greeks_v2()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

